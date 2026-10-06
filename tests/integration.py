#!/usr/bin/env python3
"""HTTP tests with real subprocesses simulating all three CLIs. Python standard library only."""
import base64, hashlib, http.client, json, os, pathlib, socket, subprocess, sys, tempfile, time, urllib.request, urllib.error
ROOT=pathlib.Path(__file__).resolve().parents[1]
BINARY=pathlib.Path(sys.argv[1]).resolve() if len(sys.argv)>1 else ROOT/'target/release/customremote'
checks=0

def verify(ok, label):
    global checks
    assert ok, label
    checks+=1
    print('OK', label, flush=True)

with tempfile.TemporaryDirectory(prefix='customremote-integration-') as tmp:
    data=pathlib.Path(tmp)
    with socket.socket() as sock: sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
    base=f'http://127.0.0.1:{port}'
    salt='00'*16
    (data/'admin.json').write_text(json.dumps({'username':'admin','salt':salt,'hash':hashlib.scrypt(b'fixture-password-2026',salt=bytes.fromhex(salt),n=16384,r=8,p=1).hex(),'revision':'python-record'}))
    fixture=str(ROOT/'tests/fixtures/cli.py')
    env={**os.environ,'REMOTE_DATA':tmp,'REMOTE_PORT':str(port),'REMOTE_SYSTEM_ACCOUNTS':'claude,codex,antigravity','REMOTE_TOKEN':'fixture-master','CLAUDE_BIN':fixture,'CODEX_BIN':fixture,'ANTIGRAVITY_BIN':fixture,'CODEX_HOME':str(data/'codex'),'ANTIGRAVITY_HOME':str(data/'antigravity'),'REMOTE_ENABLE_SESSIONS':'1','REMOTE_USER_RATE':'200/h','REMOTE_REQUEST_TIMEOUT':'2'}
    log=open(data/'server.log','w+')
    proc=subprocess.Popen([str(BINARY),'serve'],cwd=ROOT,env=env,stdout=log,stderr=log)
    cookie=''
    def req(path,method='GET',body=None,key=None,admin=False,expect=200,raw=False):
        headers={}
        if body is not None: headers['Content-Type']='application/json'
        if key: headers['Authorization']='Bearer '+key
        if admin: headers.update({'Cookie':cookie,'X-Admin':'1','Origin':base})
        r=urllib.request.Request(base+path, data=json.dumps(body).encode() if body is not None else None,headers=headers,method=method)
        try: response=urllib.request.urlopen(r,timeout=15)
        except urllib.error.HTTPError as e: response=e
        payload=response.read(); status=response.status
        assert status==expect,(path,status,payload[:1000])
        return (payload.decode() if raw else json.loads(payload)),response.headers
    try:
        for _ in range(100):
            try: req('/healthz'); break
            except Exception:
                if proc.poll() is not None: raise RuntimeError('Server exited')
                time.sleep(.05)
        _,headers=req('/admin/auth/password','POST',{'username':'admin','password':'fixture-password-2026'},admin=True)
        cookie=headers['Set-Cookie'].split(';')[0]
        verify(bool(cookie),'Python password record compatible with Rust')
        key=req('/admin/api/keys','POST',{'name':'integration'},admin=True)[0]['key']
        key2=req('/admin/api/keys','POST',{'name':'second'},admin=True)[0]['key']
        models=req('/v1/models',key=key)[0]['data']
        verify(any(m['id']=='system-codex/gpt-test' for m in models),'Codex JSON-RPC catalog')
        for model in ['system-claude/haiku','system-codex/gpt-test','system-antigravity/gemini-3-flash']:
            b={'model':model,'messages':[{'role':'user','content':'Hello'}]}
            out=req('/v1/chat/completions','POST',b,key=key)[0]
            verify(out['choices'][0]['message']['content']=='Hello 🌍!',model+' non-stream')
            verify(out['usage']['prompt_tokens']==12 and out['usage']['completion_tokens']==5,model+' usage')
            stream,h=req('/v1/chat/completions','POST',{**b,'stream':True,'stream_options':{'include_usage':True}},key=key,raw=True)
            chunks=[json.loads(line[6:]) for line in stream.splitlines() if line.startswith('data: ') and line!='data: [DONE]']
            verify('[DONE]' in stream and ''.join(c['choices'][0]['delta'].get('content','') for c in chunks if c['choices'])=='Hello 🌍!',model+' SSE')
            verify('Content-Encoding' not in h,'SSE without buffered compression')
        verify(not list((data/'antigravity').glob('**/*.pb')),'Antigravity cleanup after CLI exit')
        b={'model':'system-claude/haiku','messages':[{'role':'user','content':'TOOLS_TEST'}],'tools':[{'type':'function','function':{'name':'weather','parameters':{'type':'object'}}}]}
        out=req('/v1/chat/completions','POST',b,key=key)[0]
        verify(out['choices'][0]['message']['tool_calls'][0]['function']['name']=='weather','tool calls')
        out=req('/v1/chat/completions','POST',{'model':'system-claude/haiku','messages':[{'role':'user','content':'JSON_TEST'}],'response_format':{'type':'json_object'}},key=key)[0]
        verify(json.loads(out['choices'][0]['message']['content'])=={'ok':True},'JSON mode')
        b={'model':'system-claude/haiku','messages':[{'role':'user','content':'Hello'}],'n':3}
        verify(len(req('/v1/chat/completions','POST',b,key=key)[0]['choices'])==3,'n concurrent responses')
        out=req('/v1/chat/completions','POST',{**b,'n':1,'stop':'🌍'},key=key)[0]
        verify(out['choices'][0]['message']['content']=='Hello ','stop Unicode')
        r=req('/v1/responses','POST',{'model':'system-codex/gpt-test','input':'Hello'},key=key)[0]
        verify(r['status']=='completed','Responses completed')
        req('/v1/responses','POST',{'model':'system-codex/gpt-test','input':'Continue','previous_response_id':r['id']},key=key)
        req('/v1/responses','POST',{'model':'system-codex/gpt-test','input':'Unauthorized access','previous_response_id':r['id']},key=key2,expect=404)
        verify(True,'Responses history isolated by key')
        r,_=req('/v1/responses','POST',{'model':'system-codex/gpt-test','input':'Hello','stream':True},key=key,raw=True)
        verify('event: response.completed' in r and 'response.output_text.delta' in r,'Responses SSE')
        r=req('/v1/completions','POST',{'model':'system-claude/haiku','prompt':'Hi','echo':True},key=key)[0]
        verify(r['choices'][0]['text']=='HiHello 🌍!','legacy completions echo')
        r=req('/api/sessions','POST',{'cwd':tmp,'name':'QA'},key='fixture-master')[0]; sid=r['id']
        req(f'/api/sessions/{sid}/messages','POST',{'text':'Hello'},key='fixture-master')
        for _ in range(50):
            r=req(f'/api/sessions/{sid}',key='fixture-master')[0]
            # The pending request can become visible just before the status update.
            if r['session']['status']=='waiting' and 'permission1' in r['session']['pending']: break
            time.sleep(.05)
        verify(r['session']['status']=='waiting' and 'permission1' in r['session']['pending'],'session requests permission')
        req(f'/api/sessions/{sid}/permissions/permission1','POST',{'behavior':'allow','always':True},key='fixture-master')
        for _ in range(50):
            r=req(f'/api/sessions/{sid}',key='fixture-master')[0]
            if r['session']['turns']==1: break
            time.sleep(.05)
        verify(r['session']['turns']==1 and r['session']['cost_usd']==.001,'session result and cost')
        req(f'/api/sessions/{sid}/stop','POST',{},key='fixture-master')
        verify(req(f'/api/sessions/{sid}',key='fixture-master')[0]['session']['alive']==False,'session process stopped')
        req('/v1/chat/completions','POST',{'model':'system-claude/haiku','messages':[{'role':'user','content':[{'type':'image_url','image_url':{'url':'http://127.0.0.1/secret'}}]}]},key=key,expect=400)
        verify(True,'local image SSRF rejected')
        req('/v1/chat/completions','POST',{'model':'system-claude/haiku','messages':[{'role':'user','content':'QUOTA_TEST'}]},key=key,expect=429)
        verify(req('/admin/api/state',admin=True)[0]['accounts']['claude'][0]['status']=='paused','quota pauses the account')
        started=time.monotonic()
        req('/v1/chat/completions','POST',{'model':'system-claude/haiku','messages':[{'role':'user','content':'SLOW_TEST'}]},key=key,expect=504)
        verify(time.monotonic()-started<5,'CLI timeout is bounded')
        pidfile=data/'cancel.pid'
        payload=json.dumps({'model':'system-claude/haiku','stream':True,'messages':[{'role':'user','content':f'SLOW_TEST PIDFILE={pidfile} '}]}).encode()
        connection=socket.create_connection(('127.0.0.1',port))
        connection.sendall((f'POST /v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {key}\r\nContent-Type: application/json\r\nContent-Length: {len(payload)}\r\n\r\n').encode()+payload)
        connection.recv(4096)
        for _ in range(40):
            if pidfile.exists(): break
            time.sleep(.025)
        verify(pidfile.exists(),'process started for the cancellation test')
        pid=int(pidfile.read_text());connection.close()
        alive=True
        for _ in range(40):
            try: os.kill(pid,0)
            except ProcessLookupError: alive=False;break
            time.sleep(.025)
        verify(not alive,'SSE disconnect terminates the CLI process')
        # Small real PDF: Poppler extracts text and renders the sparse page as PNG.
        objects=[b'<< /Type /Catalog /Pages 2 0 R >>',b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>',b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>']
        content=b'BT /F1 12 Tf 40 240 Td (PDF_TEST) Tj ET'
        objects.append(b'<< /Length '+str(len(content)).encode()+b' >>\nstream\n'+content+b'\nendstream')
        pdf=b'%PDF-1.4\n'; offsets=[0]
        for i,obj in enumerate(objects,1):offsets.append(len(pdf));pdf+=str(i).encode()+b' 0 obj\n'+obj+b'\nendobj\n'
        start=len(pdf);pdf+=b'xref\n0 6\n0000000000 65535 f \n'+b''.join(f'{off:010d} 00000 n \n'.encode() for off in offsets[1:])+b'trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n'+str(start).encode()+b'\n%%EOF'
        r=req('/v1/chat/completions','POST',{'model':'system-codex/gpt-test','messages':[{'role':'user','content':[{'type':'file','file':{'filename':'test.pdf','file_data':'data:application/pdf;base64,'+base64.b64encode(pdf).decode()}}]}]},key=key)[0]
        verify(r['choices'][0]['message']['content']=='Hello 🌍!','real PDF converted by PDF tools')
        created=subprocess.check_output([str(BINARY),'create-key','--name','CLI'],env=env,text=True).strip()
        verify(req('/v1/models',key=created)[0]['object']=='list','create-key command without concurrent writes')
        proc.terminate();proc.wait(timeout=5)
        proc=subprocess.Popen([str(BINARY),'serve'],cwd=ROOT,env=env,stdout=log,stderr=log)
        for _ in range(100):
            try: req('/healthz');break
            except Exception: time.sleep(.025)
        verify(req('/v1/models',key=key)[0]['object']=='list','key preserved after restart')
        restored=req(f'/api/sessions/{sid}',key='fixture-master')[0]['session']
        verify(restored['turns']==1 and not restored['alive'],'session preserved after restart')
        connection=http.client.HTTPConnection('127.0.0.1',port,timeout=5)
        connection.request('GET','/api/events',headers={'Authorization':'Bearer fixture-master'})
        response=connection.getresponse();assert response.status==200
        proc.terminate();proc.wait(timeout=3);connection.close()
        verify(True,'graceful shutdown with an open SSE stream')
        print(f'{checks} checks passed.')
    except Exception:
        log.flush(); log.seek(0); print(log.read(),file=sys.stderr); raise
    finally:
        proc.terminate()
        try: proc.wait(timeout=10)
        except subprocess.TimeoutExpired: proc.kill(); proc.wait()
        log.close()

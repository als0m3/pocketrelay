#!/usr/bin/env python3
"""Deterministic CLI for protocol tests. No network calls."""
import json, os, sys, uuid, time, pathlib

def emit(v):
    print(json.dumps(v), flush=True)

def reply(prompt):
    if 'TOOLS_TEST' in prompt:
        return '<tool_call>{"name":"weather","arguments":{"city":"Paris"}}</tool_call>'
    if 'JSON_TEST' in prompt:
        return '```json\n{"ok":true}\n```'
    return 'Hello 🌍!'

if 'app-server' in sys.argv:
    for line in sys.stdin:
        obj = json.loads(line); mid = obj.get('id'); method = obj.get('method'); p = obj.get('params', {})
        result = {}
        if method == 'model/list': result = {'data':[{'id':'gpt-test','displayName':'GPT test','isDefault':True}]}
        elif method == 'account/read': result = {'account':{'email':'fixture@example.test','planType':'test'}}
        elif method == 'thread/start': result = {'thread':{'id':str(uuid.uuid4())}}
        elif method == 'account/login/start': result = {'verificationUrl':'https://example.test','userCode':'TEST-1234'}
        if mid is not None: emit({'id':mid,'result':result})
        if method == 'turn/start':
            tid=p['threadId']; out=reply(str(p.get('input')))
            for delta in [out[:5],out[5:]]: emit({'method':'item/agentMessage/delta','params':{'threadId':tid,'itemId':'item1','delta':delta}})
            emit({'method':'thread/tokenUsage/updated','params':{'threadId':tid,'tokenUsage':{'last':{'inputTokens':12,'cachedInputTokens':2,'outputTokens':5}}}})
            emit({'method':'turn/completed','params':{'threadId':tid,'turn':{'status':'completed'}}})
elif '--disable-slash-commands' in sys.argv:
    out=reply(sys.argv[sys.argv.index('-p')+1]); cid=str(uuid.uuid4())
    transcript=pathlib.Path(os.environ['HOME'])/'.gemini/antigravity-cli/conversations'/f'{cid}.pb'
    transcript.parent.mkdir(parents=True,exist_ok=True);transcript.write_text('test prompt')
    emit({'event':'init','conversation_id':cid,'model':'gemini-3-flash'})
    emit({'event':'step_update','text_delta':out})
    emit({'event':'result','status':'SUCCESS','response':out,'usage':{'input_tokens':12,'output_tokens':5}})
elif '--permission-prompts' in sys.argv and sys.argv[sys.argv.index('--permission-prompts')+1] == 'host':
    for line in sys.stdin:
        obj=json.loads(line)
        if obj['type']=='control_request':
            emit({'type':'control_response','response':{'subtype':'success','request_id':obj['request_id'],'response':{'commands':[], 'models':[]}}})
        elif obj['type']=='user':
            emit({'type':'system','subtype':'init','session_id':'fixture-session','model':'haiku','tools':['Read']})
            emit({'type':'control_request','request_id':'permission1','request':{'subtype':'can_use_tool','tool_name':'Read','input':{'file_path':'test.txt'}}})
        elif obj['type']=='control_response':
            emit({'type':'assistant','message':{'role':'assistant','content':[{'type':'text','text':'Permission handled'}],'usage':{'input_tokens':12}}})
            emit({'type':'result','subtype':'success','is_error':False,'total_cost_usd':0.001,'duration_ms':10})
elif '--output-format' in sys.argv and sys.argv[sys.argv.index('--output-format')+1]=='json':
    emit({'type':'result','result':'pong','is_error':False})
else:
    obj=json.loads(sys.stdin.readline()); prompt=json.dumps(obj); out=reply(prompt)
    if 'QUOTA_TEST' in prompt:
        emit({'type':'result','is_error':True,'result':'rate limit exceeded'}); sys.exit()
    if 'SLOW_TEST' in prompt:
        # A local marker lets the test verify process termination on disconnect.
        for b in obj['message']['content']:
            if b.get('type')=='text' and 'PIDFILE=' in b.get('text',''):
                path=b['text'].split('PIDFILE=')[1].split()[0]
                with open(path,'w') as f: f.write(str(os.getpid()))
        time.sleep(120)
    emit({'type':'system','subtype':'init','model':'haiku'})
    for delta in [out[:5],out[5:]]: emit({'type':'stream_event','event':{'delta':{'type':'text_delta','text':delta}}})
    emit({'type':'result','is_error':False,'result':out,'usage':{'input_tokens':10,'cache_read_input_tokens':2,'output_tokens':5}})

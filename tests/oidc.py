#!/usr/bin/env python3
"""Simulated local SSO: RSA signature, PKCE, nonce, role, expiration and public origin."""
import base64,hashlib,http.client,http.server,json,os,pathlib,socket,subprocess,tempfile,threading,time,urllib.parse
ROOT=pathlib.Path(__file__).resolve().parents[1]
def b64(v):return base64.urlsafe_b64encode(v).decode().rstrip('=')
with tempfile.TemporaryDirectory(prefix='customremote-oidc-') as tmp:
 path=pathlib.Path(tmp);key=path/'test.pem'
 subprocess.run(['openssl','genrsa','-out',str(key),'2048'],check=True,stderr=subprocess.DEVNULL)
 modulus=subprocess.check_output(['openssl','rsa','-in',str(key),'-noout','-modulus'],stderr=subprocess.DEVNULL).decode().split('=')[1].strip()
 jwks={'keys':[{'kty':'RSA','kid':'qa','alg':'RS256','use':'sig','n':b64(bytes.fromhex(modulus)),'e':'AQAB'}]}
 state={};checks=[]
 class Provider(http.server.BaseHTTPRequestHandler):
  def log_message(self,*args):pass
  def send(self,v,status=200):
   data=json.dumps(v).encode();self.send_response(status);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
  def do_GET(self):
   if self.path.endswith('/.well-known/openid-configuration'):self.send({'issuer':issuer,'authorization_endpoint':issuer+'/authorize','token_endpoint':issuer+'/token','jwks_uri':issuer+'/keys'})
   elif self.path=='/keys':self.send(jwks)
   else:self.send({},404)
  def do_POST(self):
   form=urllib.parse.parse_qs(self.rfile.read(int(self.headers['Content-Length'])).decode());verifier=form.get('code_verifier',[''])[0]
   if b64(hashlib.sha256(verifier.encode()).digest())!=state['challenge']:return self.send({'error':'bad PKCE'},400)
   claims={'iss':issuer,'sub':'fixture','aud':'client','exp':int(time.time())+300,'nonce':state['nonce'],'groups':['admins'],'email':'fixture@example.test','email_verified':True,**state.get('override',{})}
   payload=b64(b'{"alg":"RS256","kid":"qa"}')+'.'+b64(json.dumps(claims).encode())
   signature=subprocess.run(['openssl','dgst','-sha256','-sign',str(key)],input=payload.encode(),stdout=subprocess.PIPE,check=True).stdout
   self.send({'id_token':payload+'.'+b64(signature),'access_token':'test','token_type':'Bearer'})
 provider=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider);issuer=f'http://127.0.0.1:{provider.server_port}'
 threading.Thread(target=provider.serve_forever,daemon=True).start()
 with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
 env={**os.environ,'REMOTE_DATA':str(path/'data'),'REMOTE_PORT':str(port),'REMOTE_OIDC_ISSUER':issuer,'REMOTE_OIDC_CLIENT_ID':'client','REMOTE_ADMIN_GROUPS':'admins','REMOTE_ADMIN_EMAILS':'','REMOTE_PUBLIC_URL':'https://public.example.test','REMOTE_SYSTEM_ACCOUNTS':'','REMOTE_ENABLE_CODEX':'0','REMOTE_ENABLE_ANTIGRAVITY':'0'}
 proc=subprocess.Popen([str(ROOT/'target/release/customremote'),'serve'],cwd=ROOT,env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
 def request(path,headers={},method='GET',body=None):
  c=http.client.HTTPConnection('127.0.0.1',port,timeout=5);c.request(method,path,body=json.dumps(body) if body is not None else None,headers=headers);r=c.getresponse();headers=dict(r.getheaders());headers['cookies']=[v for k,v in r.getheaders() if k.lower()=='set-cookie'];out=(r.status,headers,r.read());c.close();return out
 try:
  for _ in range(100):
   try:request('/healthz');break
   except OSError:time.sleep(.02)
  for label,override,wanted in [('valid',{},303),('future nbf',{'nbf':int(time.time())+300},403),('malformed nbf',{'nbf':str(int(time.time())+300)},403),('malformed exp',{'exp':'never'},403),('nonce',{'nonce':'wrong'},403),('expiration',{'exp':int(time.time())-120},403),('role',{'groups':['users']},403)]:
   r=request('/admin/auth/login');assert r[0]==303,r
   q=urllib.parse.parse_qs(urllib.parse.urlparse(r[1]['location']).query);state.update(nonce=q['nonce'][0],challenge=q['code_challenge'][0],override=override)
   cookie=r[1]['set-cookie'].split(';')[0];callback='/admin/auth/callback?'+urllib.parse.urlencode({'state':q['state'][0],'code':'test'})
   assert request(callback)[0]==403,'OIDC state not bound to the browser'
   r=request(callback,{'Cookie':cookie});assert r[0]==wanted,(label,r)
   if wanted==303:
    admin_cookie=next(c for c in r[1]['cookies'] if c.startswith('cr_admin=')).split(';')[0]
    assert r[1]['location']=='/admin'
    assert request('/admin/api/state',{'Cookie':admin_cookie})[0]==200
   assert request(callback,{'Cookie':cookie})[0]==403,'replay accepted'
   checks.append(label)
  r=request('/admin/auth/config',{'Host':'public.example.test'});cfg=json.loads(r[2]);assert cfg['oidc'] and not cfg['token_login'] and not cfg['password_login']
  assert request('/admin/auth/token',{'Host':'public.example.test','X-Admin':'1'},'POST',{'token':'anything'})[0]==403
  print('SSO OK: PKCE, state cookie, RSA signature, nonce, expiration, not-before and malformed dates, role, replay prevention and required SSO on the public domain.')
 finally:
  proc.terminate();proc.wait(timeout=10);provider.shutdown();provider.server_close()

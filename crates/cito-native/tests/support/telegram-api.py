# purpose: provide a loopback-only Bot API for the persistent channel contract test.
# usage: python3 telegram-api.py; prints the ephemeral port on stdout.
# effects: appends sent messages to temporary HOME/api-sends.jsonl.
# requires: Python 3 and a cleanroom HOME.
import http.server
import json
import os
import pathlib

sent = []
polls = 0
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def do_POST(self):
        global polls
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        method = self.path.split('/')[-1]
        result = True
        if method == 'getMe':
            result = dict(username='example_bot')
        elif method == 'sendMessage':
            sent.append(body)
            with (pathlib.Path(os.environ['HOME']) / 'api-sends.jsonl').open('a') as log:
                log.write(json.dumps(body) + '\n')
            result = dict(message_id=100 + len(sent))
        elif method == 'getUpdates':
            polls += 1
            result = [dict(update_id=12, message=dict(chat=dict(id=9, type='private'), **{'from': dict(id=9)}, text='/start cito-abcdefghij')),dict(update_id=15,message=dict(chat=dict(id=9,type='private'),**{'from':dict(id=9)},text='ship',reply_to_message=dict(message_id=101)))] if polls == 1 else []
        data = json.dumps(dict(ok=True,result=result)).encode()
        self.send_response(200)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)
server = http.server.HTTPServer(('127.0.0.1',0),Handler)
print(json.dumps(dict(port=server.server_port)), flush=True)
server.serve_forever()

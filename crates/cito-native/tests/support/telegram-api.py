# purpose: provide a loopback-only Bot API for the persistent channel contract test.
# usage: python3 telegram-api.py; prints the ephemeral port on stdout.
# effects: appends sent messages to temporary HOME/api-sends.jsonl.
# requires: Python 3 and a cleanroom HOME.
import http.server
import json
import os
import pathlib
import socketserver

sent = []
polls = 0
attempts = {}
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def do_POST(self):
        global polls
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        method = self.path.split('/')[-1]
        with (pathlib.Path(os.environ['HOME']) / 'api-methods.jsonl').open('a') as log:
            log.write(json.dumps(method) + '\n')
        token = self.path.split('/')[1]
        if method == 'getUpdates' and token in ('botpoll409', 'botpoll401'):
            status = int(token[-3:])
            data = json.dumps(dict(ok=False, description='Polling rejected by fixture', error_code=status)).encode()
            self.send_response(status)
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        result = True
        if method == 'getMe':
            result = dict(username='example_bot')
        elif method == 'sendMessage':
            chat = str(body['chat_id'])
            attempts[chat] = attempts.get(chat, 0) + 1
            status = int(chat.split(':')[1]) if chat.startswith('status:') else (429 if chat == 'partial' and attempts[chat] == 2 else None)
            if status:
                data = json.dumps(dict(ok=False, description='Rejected by fixture', error_code=status)).encode()
                self.send_response(status)
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                self.wfile.write(data)
                return
            if chat == 'uncertain':
                self.close_connection = True
                self.connection.shutdown(2)
                return
            sent.append(body)
            with (pathlib.Path(os.environ['HOME']) / 'api-sends.jsonl').open('a') as log:
                log.write(json.dumps(body) + '\n')
            result = dict(message_id=100 + len(sent))
        elif method == 'getUpdates':
            polls += 1
            with (pathlib.Path(os.environ['HOME']) / 'api-polls.jsonl').open('a') as log:
                log.write(json.dumps(dict(path=self.path, **body)) + '\n')
            result = [dict(update_id=12, message=dict(chat=dict(id=9, type='private'), **{'from': dict(id=9)}, text='/start cito-abcdefghij')),dict(update_id=15,message=dict(chat=dict(id=9,type='private'),**{'from':dict(id=9)},text='ship',reply_to_message=dict(message_id=101)))] if polls == 1 else []
        data = json.dumps(dict(ok=True,result=result)).encode()
        self.send_response(200)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)
# A loopback fixture must not perform HTTPServer's reverse-DNS lookup at bind time.
server = socketserver.TCPServer(('127.0.0.1',0),Handler)
print(json.dumps(dict(port=server.server_address[1])), flush=True)
server.serve_forever()

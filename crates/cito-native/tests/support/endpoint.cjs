// purpose: expose a real live endpoint and then disconnect its bridge for cached-PID tests.
// usage: node <recognized plugin entrypoint>; Rust controls stdin with NDJSON.
// effects: a private Unix socket and stdout receipts only; cleans the socket on exit.
// requires: Node.js, FIXTURE_HARNESS and cleanroom CITO_STATE_DIR.
const net=require('node:net'), fs=require('node:fs'), path=require('node:path'), readline=require('node:readline');
const harness=process.env.FIXTURE_HARNESS;
const socket=path.join(process.env.CITO_STATE_DIR,'sockets',`${harness}-pid-${process.pid}.sock`);
fs.mkdirSync(path.dirname(socket),{recursive:true});
let open=true;
const server=net.createServer(stream=>{let data='';stream.on('data',chunk=>{data+=chunk;if(!data.includes('\n'))return;stream.end(JSON.stringify(open?{ok:true,session_id:'session-a',cwd:'/repo',session_name:'Fixture',endpoint_pid:process.pid}:{ok:false,error:'SESSION_NOT_OPEN'})+'\n');});});
server.listen(socket,()=>console.log(JSON.stringify({op:'ready'})));
readline.createInterface({input:process.stdin}).on('line',line=>{const req=JSON.parse(line);if(req.op==='offline')server.close(()=>console.log(JSON.stringify({op:'offline'})));if(req.op==='closed'){open=false;console.log(JSON.stringify({op:'closed'}));}});
setInterval(()=>{},1000);
process.on('exit',()=>{try{fs.unlinkSync(socket)}catch{}});

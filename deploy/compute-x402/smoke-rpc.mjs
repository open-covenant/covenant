// Isolated container fixture: it can identify mainnet but cannot forward or send.
import http from 'node:http';
import {pathToFileURL} from 'node:url';
export const MAINNET_GENESIS='5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d';
export function createSmokeRpc(genesis){
  if(genesis!==MAINNET_GENESIS)throw Error('Mainnet genesis observation required');
  const counts={genesis:0,refused:0,coordinator:0};
  const server=http.createServer(async(req,res)=>{
    const reply=(status,value)=>{res.writeHead(status,{'content-type':'application/json'});res.end(JSON.stringify(value));};
    if(req.method==='GET'&&req.url==='/audit')return reply(200,counts);
    if(req.url?.startsWith('/coordinator')){counts.coordinator++;return reply(503,{error:'No dispatch allowed in container smoke'});}
    try{
      if(req.method!=='POST'||req.url!=='/rpc')throw Error();
      let size=0,body='';for await(const chunk of req){size+=chunk.length;if(size>4096)throw Error();body+=chunk;}
      const value=JSON.parse(body);
      if(value.jsonrpc!=='2.0'||value.method!=='getGenesisHash'||!Array.isArray(value.params)||value.params.length)throw Error();
      counts.genesis++;return reply(200,{jsonrpc:'2.0',id:value.id,result:genesis});
    }catch{counts.refused++;return reply(403,{error:'Only read-only genesis identification is allowed'});}
  });
  server.requestTimeout=5000;server.headersTimeout=5000;
  return {server,counts};
}
if(process.argv[1]&&import.meta.url===pathToFileURL(process.argv[1]).href){
  const {server}=createSmokeRpc(process.env.SMOKE_GENESIS);
  server.on('error',error=>{process.stderr.write(JSON.stringify({fixture:'rpc',error:error.code??'startup_failed'})+'\n');process.exitCode=1;});
  server.listen(14045,'0.0.0.0',()=>process.stdout.write(JSON.stringify({fixture:'rpc',status:'listening',port:14045})+'\n'));
}

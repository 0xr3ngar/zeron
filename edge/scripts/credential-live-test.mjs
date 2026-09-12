// Real-worker transport for the Rust multi-device tests; no deployment or keys.
import { build } from 'esbuild';
import { Miniflare } from 'miniflare';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
const directory = await mkdtemp(join(tmpdir(), 'zeron-credential-test-'));
let worker;
try {
  await build({entryPoints:['src/index.ts'], outfile:join(directory,'worker.mjs'), bundle:true, format:'esm', platform:'browser', alias:{'loro-crdt':'loro-crdt/base64'}, loader:{'.sh':'text'}, logLevel:'warning'});
  worker = new Miniflare({modules:true,modulesRoot:directory,scriptPath:join(directory,'worker.mjs'),compatibilityDate:'2026-07-01',compatibilityFlags:['nodejs_compat'],host:'127.0.0.1',port:0,
    durableObjects:Object.fromEntries([['VAULT_ROOMS','VaultRoom'],['SESSION_ROOMS','SessionRoom'],['DEVICE_ROOMS','DeviceRoom'],['PREVIEW_ROOMS','PreviewRoom'],['REGISTRY_ROOMS','RegistryRoom'],['CHAT_ROOMS','ChatRoom']].map(([name,className])=>[name,{className,useSQLite:true}])),
    r2Buckets:['BLOBS','RELEASES'],bindings:{AUTH_MODE:'dev',WORKOS_CLIENT_ID:'test'}});
  const address = await worker.ready;
  console.log(`Testing credential vault against ${address.origin}`);
  const code = await new Promise((resolve,reject)=>{
    const child=spawn('cargo',['test','-p','zeron-engine','--test','credential_vault_e2e','--','--nocapture'],{cwd:'..',stdio:'inherit',env:{...process.env,ZERON_VAULT_EDGE_URL:address.origin}});
    child.on('error',reject);child.on('close',resolve);
  });
  process.exitCode=code??1;
} finally {await worker?.dispose();await rm(directory,{recursive:true,force:true});}

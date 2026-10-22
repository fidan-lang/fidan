// Local immutable-manifest rehearsal. No production URLs are served or modified.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {createServer} from 'node:http';
import {spawn,spawnSync} from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const [compilerArg,fragmentArg,payloadArg,outputArg]=process.argv.slice(2);
assert(compilerArg&&fragmentArg&&payloadArg&&outputArg,'Usage: node scripts/rehearse-release.mjs <candidate-cli> <helper-fragment> <payload-root> <new-target-output>');
const compiler=path.resolve(compilerArg), output=path.resolve(outputArg);
assert(output.startsWith(path.join(root,'target')+path.sep),'Output must remain in this checkout target/');
fs.mkdirSync(output); // Never overwrite a previous rehearsal.
const candidate=JSON.parse(fs.readFileSync(path.join(root,'config/release-candidate.json'),'utf8'));
const fragment=JSON.parse(fs.readFileSync(path.resolve(fragmentArg),'utf8').replace(/^\uFEFF/,''));
const helper=fragment.toolchains[0];
assert.equal(helper.backend_protocol_version,candidate.llvm_backend_protocol);
assert.equal(helper.toolchain_version,candidate.llvm_helper_version);
const archive=path.join(path.resolve(payloadArg),'toolchains/llvm',helper.toolchain_version,helper.host_triple,path.basename(new URL(helper.url).pathname));
const digest=file=>createHash('sha256').update(fs.readFileSync(file)).digest('hex');
assert.equal(digest(archive),helper.sha256);
const stage=path.join(output,'compiler');fs.mkdirSync(stage);
const binary=process.platform==='win32'?'fidan.exe':'fidan';
fs.copyFileSync(compiler,path.join(stage,binary));
for(const name of ['LICENSE','NOTICE'])fs.copyFileSync(path.join(root,name),path.join(stage,name));
// Include the matching source ABI runtime required for native link tests.
for(const name of fs.readdirSync(path.join(root,'target/debug')).filter(n=>/^(?:lib)?fidan_runtime\.(?:lib|dll|dll.lib|a|so|dylib)$/.test(n)))
 fs.copyFileSync(path.join(root,'target/debug',name),path.join(stage,name));
const compilerArchive=path.join(output,'compiler.tar.gz');
assert.equal(spawnSync('tar',['-czf',compilerArchive,'-C',stage,'.'],{windowsHide:true}).status,0);
const entry={version:candidate.compiler_version,host_triple:helper.host_triple,binary_relpath:binary,sha256:digest(compilerArchive),url:`https://candidate.invalid/fidan/${candidate.compiler_version}/${helper.host_triple}/compiler.tar.gz`};
const immutable={schema_version:1,fidan_versions:[entry],toolchains:[helper]};
fs.writeFileSync(path.join(output,'proposed-manifest.json'),JSON.stringify(immutable,null,2));
const home=path.join(output,'home');
const profile=path.join(output,'profile'),local=path.join(output,'localappdata');
fs.mkdirSync(profile);fs.mkdirSync(local);
const installRoot=process.platform==='win32'?path.join(local,'Programs/Fidan'):process.platform==='darwin'?path.join(profile,'Applications/Fidan'):path.join(output,'xdg/fidan/installs');
const activeFile=path.join(installRoot,'metadata/active-version.json');
const isolated={...process.env,FIDAN_HOME:home,FIDAN_DIST_MANIFEST:'',LOCALAPPDATA:local,USERPROFILE:profile,HOME:profile,XDG_DATA_HOME:path.join(output,'xdg')};
let served=structuredClone(immutable);
const server=createServer((req,res)=>{
 if(req.url==='/manifest.json'){res.setHeader('Content-Type','application/json');res.end(JSON.stringify(served));}
 else if(req.url==='/compiler.tar.gz')fs.createReadStream(compilerArchive).pipe(res);
 else if(req.url==='/helper.tar.gz')fs.createReadStream(archive).pipe(res);
 else {res.statusCode=404;res.end();}
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const base=`http://127.0.0.1:${server.address().port}`;
served.fidan_versions[0].url=base+'/compiler.tar.gz';served.toolchains[0].url=base+'/helper.tar.gz';
async function run(exe,args,allowed=0){
 const child=spawn(exe,args,{cwd:root,env:{...isolated,FIDAN_DIST_MANIFEST:base+'/manifest.json'},windowsHide:true,signal:AbortSignal.timeout(120000),stdio:['ignore','pipe','pipe']});
 let out='',err='';child.stdout.on('data',v=>out+=v);child.stderr.on('data',v=>err+=v);
 const status=await new Promise((resolve,reject)=>{child.once('error',reject);child.once('close',resolve);});
 if(allowed===0)assert.equal(status,0,`${args.join(' ')}: ${err}`);else assert.notEqual(status,0);
 return out+err;
}
try{
 assert((await run(compiler,['--version'])).includes(candidate.compiler_version));
 await run(compiler,['self','install',candidate.compiler_version]);
 await run(compiler,['self','use',candidate.compiler_version]);
 await run(compiler,['toolchain','add','llvm','--version',candidate.llvm_helper_version]);
 const installed=path.join(home,'toolchains/llvm',helper.host_triple,helper.toolchain_version);
 const meta=JSON.parse(fs.readFileSync(path.join(installed,'metadata.json'),'utf8'));
 assert.equal(meta.backend_protocol_version,candidate.llvm_backend_protocol);
 for(const name of ['LICENSE','NOTICE'])assert.equal(digest(path.join(installed,name)),digest(path.join(root,name)));
 const before=JSON.parse(fs.readFileSync(activeFile,"utf8")).active_version;
 const installedCompiler=path.join(installRoot,"versions",candidate.compiler_version,binary);
 const beforeBinary=digest(installedCompiler);
 served.fidan_versions[0].version='1.1.0-rc.2';served.fidan_versions[0].sha256='0'.repeat(64);
 assert.match(await run(compiler,['self','install','1.1.0-rc.2'],1),/checksum|sha-?256|hash/i);
 assert.equal(JSON.parse(fs.readFileSync(activeFile,'utf8')).active_version,before,'Failed install changed active version');
 assert.equal(digest(installedCompiler),beforeBinary,'Failed install replaced the active binary');
 served.toolchains[0].toolchain_version='1.1.0-rc.2';served.toolchains[0].sha256='0'.repeat(64);
 assert.match(await run(compiler,['toolchain','add','llvm','--version','1.1.0-rc.2'],1),/checksum|sha-?256|hash/i);
 assert.equal(JSON.parse(fs.readFileSync(path.join(installed,'metadata.json'),'utf8')).backend_protocol_version,8);
 const smoke=path.join(output,'smoke.fdn');fs.writeFileSync(smoke,'assert_eq(2 ** -3, 0.125)\nprint("candidate protocol eight")\n');
 const native=path.join(output,process.platform==='win32'?'smoke.exe':'smoke');
 await run(installedCompiler,['build','--backend','llvm','--lto','full',smoke,'-o',native]);
 assert.match(await run(native,[]),/candidate protocol eight/);
 const requestFile=path.join(output,'wrong-protocol-request.json'),responseFile=path.join(output,'wrong-protocol-response.json');
 fs.writeFileSync(requestFile,JSON.stringify({protocol_version:5,input:smoke,output:native,runtime_dir:path.dirname(installedCompiler),payload:{program:{functions:[],objects:[],enums:[],use_decls:[],globals:[],test_functions:[],namespace_global_count:0},symbols:[]},opt_level:'o2',lto:'full',strip:'off',emit_obj:false,extra_lib_dirs:[],link_dynamic:false,target_cpu:null}));
 await run(path.join(installed,helper.helper_relpath),['compile','--request',requestFile,'--response',responseFile]);
 const rejected=JSON.parse(fs.readFileSync(responseFile,'utf8'));
 assert.equal(rejected.success,false);assert.equal(rejected.protocol_version,8);assert.match(rejected.message,/protocol mismatch/i);
 // Metadata mismatch must be rejected before invoking incompatible helper code.
 fs.writeFileSync(path.join(installed,'metadata.json'),JSON.stringify({...meta,backend_protocol_version:5}));
 assert.match(await run(installedCompiler,['build','--backend','llvm',smoke,'-o',native],1),/protocol|compatible|installed/i);
 fs.writeFileSync(path.join(installed,'metadata.json'),JSON.stringify(meta));
 fs.writeFileSync(path.join(output,'receipt.json'),JSON.stringify({status:'LOCAL_REHEARSAL_COMPLETE_NOT_PUBLISHED',candidate,home,compiler_sha256:digest(compiler),helper_sha256:digest(archive),checks:['compiler install/use','helper install protocol8','legal bytes','corrupt compiler rollback','corrupt helper rollback','LLVM full LTO','protocol5 request and metadata rejected']},null,2));
 console.log(`Local compiler/helper candidate rehearsal passed; home=${home}`);
}finally{await new Promise(resolve=>server.close(resolve));}

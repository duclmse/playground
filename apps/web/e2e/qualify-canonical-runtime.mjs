#!/usr/bin/env node
// Production-artifact gates. Fresh Chromium processes + fresh workers over
// localhost; these are reproducible lab limits, not Internet latency claims.
import assert from "node:assert/strict";
import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { resolve, dirname, extname } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "node:http";
import { gzipSync } from "node:zlib";
import { createHash } from "node:crypto";
import os from "node:os";
import { chromium } from "playwright";
const app = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const dist = resolve(app, process.env.SOL_QUAL_DIST ?? "dist");
const assets = readdirSync(resolve(dist,"assets"));
const wasmAssets = assets.filter((name) => name.endsWith(".wasm"));
assert.equal(wasmAssets.length,1,"production must ship exactly one runtime WASM module");
assert.match(wasmAssets[0],/^sol_bg-/);
assert.ok(!assets.some((name) => /lua_vm|piccolo/i.test(name)),"no legacy runtime asset");
const workers = assets.filter((name) => /^lua-worker-.*\.js$/.test(name));
assert.equal(workers.length,1);
const worker = readFileSync(resolve(dist,"assets",workers[0]));
const wasm = readFileSync(resolve(dist,"assets",wasmAssets[0]));
assert.ok(!worker.toString().includes("@lua-playground/runtime"));
assert.deepEqual(WebAssembly.Module.imports(new WebAssembly.Module(wasm)).filter((entry)=>entry.module==="env"),[]);
const limits = { wasmRaw:3*1024*1024,wasmGzip:900*1024,workerRaw:100*1024,initP95Ms:1000,warmRunP95Ms:25,burstP95Ms:50 };
assert.ok(wasm.length<=limits.wasmRaw); assert.ok(gzipSync(wasm).length<=limits.wasmGzip); assert.ok(worker.length<=limits.workerRaw);
const server=createServer((req,res)=>{
  const path=new URL(req.url,"http://localhost").pathname;
  if(path==="/qualify"){res.setHeader("Content-Type","text/html");res.end("<!doctype html><title>Sol qualification</title>");return;}
  const target=resolve(dist,"."+decodeURIComponent(path));
  if(!target.startsWith(dist+"/")){res.writeHead(403);res.end();return;}
  try{const bytes=readFileSync(target);res.setHeader("Content-Type",extname(target)===".wasm"?"application/wasm":extname(target)===".js"?"text/javascript":"text/html");res.end(bytes);}
  catch{res.writeHead(404);res.end();}
});
await new Promise((resolve,reject)=>{server.once("error",reject);server.listen(0,"127.0.0.1",resolve);});
const origin=`http://127.0.0.1:${server.address().port}`;
const initSamples=[];let last;let browserVersion;
const mixedFixture=(name)=>readFileSync(resolve(app,"../../crate/sol/tests/fixtures/wasm-mixed/project",name),"utf8");
const coroutineFiles={"main.sol":mixedFixture("coroutine.sol"),"runner.lua":mixedFixture("runner.lua")};
const nestedCoroutine=mixedFixture("nested_coroutine.sol");
try{
  // Each initialization sample excludes browser startup but uses a fresh
  // browser process, avoiding a shared compiled-WASM/HTTP worker cache.
  for(let trial=0;trial<10;trial++){
    const browser=await chromium.launch({args:["--no-sandbox"]});
    try{
      browserVersion=browser.version();const page=await browser.newPage();await page.goto(origin+"/qualify");
      const result=await page.evaluate(async({workerPath,lastTrial,coroutineFiles,nestedCoroutine})=>{
        const start=performance.now();const worker=new Worker(workerPath,{type:"module"});
        let pending;let id=1;
        const ready=new Promise((resolve,reject)=>{
          const timeout=setTimeout(()=>reject(new Error("worker initialization timeout")),10000);
          worker.onerror=(error)=>reject(new Error(error.message));
          worker.onmessage=({data})=>{
            if(data.type==="ready"){clearTimeout(timeout);resolve();}
            else if(pending){const request=pending;pending=null;clearTimeout(request.timeout);if(data.type==="error")request.reject(new Error(data.message));else request.resolve(data);}
          };
        });
        const rpc=(message)=>new Promise((resolve,reject)=>{const timeout=setTimeout(()=>reject(new Error(`timeout: ${message.type}`)),10000);pending={resolve,reject,timeout};worker.postMessage({...message,id:id++});});
        const check=(value,message)=>{if(!value)throw new Error(message);};
        try{
          await ready;const initMs=performance.now()-start;if(!lastTrial)return{initMs};
          const warmRuns=[];for(let sample=0;sample<25;sample++){
            const start=performance.now();const run=await rpc({type:"run",entry:"main.lua",files:{"main.lua":"print(42)"}});
            check(!run.error&&run.output==="42\n","canonical Lua output");warmRuns.push(performance.now()-start);
          }
          const files={"main.sol":"import helper\nfunction main():i64\n local base:i64=40\n local result=helper.add(base)\n return base+result\nend",
            "helper.lua":"function add(n:i64):i64\n local y=n+2\n print(y)\n return y\nend"};
          const run=await rpc({type:"run",entry:"main.sol",files});check(!run.error&&run.output==="42\n82","typed/generic run");
          await rpc({type:"debugLaunch",entry:"main.sol",files});
          check((await rpc({type:"debugSetBreakpoint",sourceId:"helper.lua",line:3})).breakpoint.verified,"mixed breakpoint verification");
          const stop=await rpc({type:"debugContinueBurst",maxInstructions:1000});check(stop.stop?.reason==="breakpoint"&&stop.source==="helper.lua","mixed live stop");
          const frames=await rpc({type:"debugGetStackTrace",threadId:0});check(frames.frames.length===2&&frames.frames[1].source==="main.sol","combined mixed frames");
          for(const [frameIndex,name,valueExpr] of [[0,"y","43"],[1,"base","1"]])check((await rpc({type:"debugSetVariable",threadId:0,frameIndex,name,valueExpr})).result.ok,"mixed live edit");
          await rpc({type:"debugForceGc"});
          const profile=await rpc({type:"profile",entry:"main.sol",files});check(profile.stats.some((stat)=>stat.functionId==="helper.add"&&stat.calls===1),"mixed profiling");
          const timeline=await rpc({type:"recordTimeline",entry:"main.sol",files,maxEvents:2});check(timeline.timeline.events.length===2&&timeline.timeline.truncated&&!timeline.timeline.error,"bounded mixed timeline");
          check((await rpc({type:"debugContinueBurst",maxInstructions:1000})).stop?.reason==="terminated","mixed resume");
          check((await rpc({type:"debugTakeOutput"})).text==="43\n44","analysis preserves edited mixed session");
          const reentrant=await rpc({type:"run",entry:"main.sol",files:{"main.sol":"function generic(n:i64):i64 print(n); if n==0 then return 40 end; return typed(n-1)+1 end\nfunction typed(n:i64):i64 return generic(n)+1 end\nfunction main():i64 return typed(1) end"}});
          check(!reentrant.error&&reentrant.output==="1\n0\n43","reentrant mixed execution");
          const caught=await rpc({type:"run",entry:"main.sol",files:{"main.sol":"function add(n:i64):i64 return n+2 end\nprint(add(40)); print(pcall(add,'bad'))"}});
          check(!caught.error&&caught.output.startsWith("42\nfalse\t"),"generic protected call catches checked bridge error");
          const coRun=await rpc({type:"run",entry:"main.sol",files:coroutineFiles});
          check(!coRun.error&&coRun.output==="40\n42\n42\t44\n45","mixed coroutine run/yield/resume");
          await rpc({type:"debugLaunch",entry:"main.sol",files:coroutineFiles});
          const coBp=(await rpc({type:"debugSetBreakpoint",sourceId:"runner.lua",line:3})).breakpoint;
          await rpc({type:"debugSetBreakpointCondition",breakpointId:coBp.id,condition:"value == 41"});
          check((await rpc({type:"debugContinueBurst",maxInstructions:1000})).stop?.reason==="breakpoint","child mixed breakpoint condition");
          const coThreads=await rpc({type:"debugGetThreads"});
          check(coThreads.threads.map((t)=>t.status).join(",")==="normal,running","mixed resume chain statuses");
          for(const [threadId,length]of [[0,2],[1,3]])check((await rpc({type:"debugGetStackTrace",threadId})).frames.length===length,"mixed thread frame isolation");
          for(const [threadId,frameIndex,name,valueExpr]of [[0,1,"base","2"],[1,0,"value","42"]])check((await rpc({type:"debugSetVariable",threadId,frameIndex,name,valueExpr})).result.ok,"mixed coroutine live edits");
          await rpc({type:"debugForceGc"});
          check((await rpc({type:"profile",entry:"main.sol",files:coroutineFiles})).stats.some((s)=>s.functionId==="typed"&&s.calls===2),"mixed coroutine profiling");
          check((await rpc({type:"debugGetUpvalues",threadId:1,frameIndex:2})).variables.some((v)=>v.name==="keep"&&v.expandable),"mixed child upvalue roots");
          check(!(await rpc({type:"recordTimeline",entry:"main.sol",files:coroutineFiles,maxEvents:2})).timeline.error,"mixed coroutine timeline");
          check((await rpc({type:"debugEvaluate",threadId:1,frameIndex:0,expression:"value"})).result.display==="42","analysis retains child edit");
          await rpc({type:"debugRemoveBreakpoint",breakpointId:coBp.id});
          check((await rpc({type:"debugContinueBurst",maxInstructions:1000})).stop?.reason==="terminated","mixed coroutine resumed to completion");
          check((await rpc({type:"debugTakeOutput"})).text==="40\n43\n43\t45\n47","mixed coroutine edited output");
          await rpc({type:"debugLaunch",entry:"main.sol",files:{"main.sol":nestedCoroutine}});
          const nestedBp=(await rpc({type:"debugSetBreakpoint",sourceId:"main.sol",line:3})).breakpoint;
          check((await rpc({type:"debugContinueBurst",maxInstructions:1000})).stop?.reason==="breakpoint","nested typed coroutine stop");
          check((await rpc({type:"debugGetThreads"})).threads.map((t)=>t.status).join(",")==="normal,normal,running","three-level mixed resume chain");
          await rpc({type:"debugRemoveBreakpoint",breakpointId:nestedBp.id});
          for(const length of [3,2]){
            check((await rpc({type:"debugStepOut"})).stop.reason==="step","step out across mixed coroutine boundary");
            check((await rpc({type:"debugGetThreads"})).threads.length===length,"step out returns to correct thread");
          }
          await rpc({type:"debugLaunch",entry:"main.sol",files:{"main.sol":"function typed(n:i64):i64 return n+1 end\nwhile true do typed(40) end"}});
          const bursts=[];for(let sample=0;sample<25;sample++){
            const start=performance.now();const burst=await rpc({type:"debugContinueBurst",maxInstructions:1000});check(!burst.stopped,"bounded runaway burst");bursts.push(performance.now()-start);
          }
          return{initMs,warmRuns,bursts};
        }finally{worker.terminate();}
      },{workerPath:"/assets/"+workers[0],lastTrial:trial===9,coroutineFiles,nestedCoroutine});
      initSamples.push(result.initMs);if(trial===9)last=result;
    }finally{await browser.close();}
  }
  const p95=(samples)=>[...samples].sort((a,b)=>a-b)[Math.ceil(samples.length*0.95)-1];
  const metrics={initP95Ms:p95(initSamples),warmRunP95Ms:p95(last.warmRuns),burstP95Ms:p95(last.bursts)};
  for(const [name,value]of Object.entries(metrics))assert.ok(value<=limits[name],`${name}: ${value} exceeds ${limits[name]}`);
  const report={status:"pass",environment:{platform:os.platform(),arch:os.arch(),cpu:os.cpus()[0]?.model,node:process.version,chromium:browserVersion,transport:"localhost HTTP; fresh browser process per initialization sample"},
    artifacts:{wasmRaw:wasm.length,wasmGzip:gzipSync(wasm).length,wasmSha256:createHash("sha256").update(wasm).digest("hex"),workerRaw:worker.length},limits,metrics,samples:{initialization:initSamples,warmRun:last.warmRuns,burst:last.bursts}};
  if(process.env.SOL_QUAL_REPORT)writeFileSync(process.env.SOL_QUAL_REPORT,JSON.stringify(report,null,2)+"\n");
  console.log(JSON.stringify(report,null,2));
}finally{server.closeAllConnections();await new Promise((resolve)=>server.close(resolve));}

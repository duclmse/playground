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
try{
  // Each initialization sample excludes browser startup but uses a fresh
  // browser process, avoiding a shared compiled-WASM/HTTP worker cache.
  for(let trial=0;trial<10;trial++){
    const browser=await chromium.launch({args:["--no-sandbox"]});
    try{
      browserVersion=browser.version();const page=await browser.newPage();await page.goto(origin+"/qualify");
      const result=await page.evaluate(async({workerPath,lastTrial})=>{
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
          await rpc({type:"debugLaunch",entry:"main.sol",files:{"main.sol":"function typed(n:i64):i64 return n+1 end\nwhile true do typed(40) end"}});
          const bursts=[];for(let sample=0;sample<25;sample++){
            const start=performance.now();const burst=await rpc({type:"debugContinueBurst",maxInstructions:1000});check(!burst.stopped,"bounded runaway burst");bursts.push(performance.now()-start);
          }
          return{initMs,warmRuns,bursts};
        }finally{worker.terminate();}
      },{workerPath:"/assets/"+workers[0],lastTrial:trial===9});
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

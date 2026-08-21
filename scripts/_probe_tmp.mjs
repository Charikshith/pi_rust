import { register } from "node:module";
import { existsSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { join, dirname } from "node:path";
const here = dirname(fileURLToPath(import.meta.url));
const piRoot = join(here, "..", "..", "pi");
const PKGS = join(piRoot, "packages");
const ROOTS = {
  "@earendil-works/pi-ai": pathToFileURL(join(PKGS,"ai","src")+"/").href,
  "@earendil-works/pi-agent-core": pathToFileURL(join(PKGS,"agent","src")+"/").href,
  "@earendil-works/pi-tui": pathToFileURL(join(PKGS,"tui","src")+"/").href,
  "@earendil-works/pi-telemetry": pathToFileURL(join(PKGS,"telemetry","src")+"/").href,
};
register("data:text/javascript,"+encodeURIComponent(`
import { existsSync } from "node:fs";
const ROOTS=${JSON.stringify(ROOTS)};
export async function resolve(specifier,context,nextResolve){
  for(const [pkg,root] of Object.entries(ROOTS)){
    if(specifier===pkg) return {url:new URL("index.ts",root).href,shortCircuit:true};
    if(specifier.startsWith(pkg+"/")){
      const rest=specifier.slice(pkg.length+1);
      for(const cand of [rest+".ts",rest+"/index.ts"]){
        const u=new URL(cand,root);
        if(existsSync(fileURLToPath(u))) return {url:u.href,shortCircuit:true};
      }
      throw new Error("no file "+specifier);
    }
  }
  return nextResolve(specifier,context);
}
`), import.meta.url);
const { getModel } = await import(pathToFileURL(join(PKGS,"ai","src","compat.ts")).href);
const { stream } = await import(pathToFileURL(join(PKGS,"ai","src","api","anthropic-messages.ts")).href);
const m = getModel("anthropic","claude-haiku-4-5");
console.log("model.id:", m.id);
// minimal fake client streaming two SSE events
class FakeClient { async *streamRaw(){ 
  yield { type:"message_start", message:{ id:"msg_test", type:"message", role:"assistant", content:[], model:m.id, stop_reason:null, stop_sequence:null, usage:{ input_tokens:12, output_tokens:5 } } };
  yield { type:"content_block_start", index:0, content_block:{ type:"text", text:"" } };
  yield { type:"content_block_delta", index:0, delta:{ type:"text_delta", text:"Hello" } };
  yield { type:"content_block_stop", index:0 };
  yield { type:"message_delta", delta:{ stop_reason:"end_turn" }, usage:{ output_tokens:5 } };
  yield { type:"message_stop" };
}}
const ctx = { messages: [{role:"user", content:"hi"}], tools: [] };
const s = stream(m, ctx, { client: new FakeClient() });
for await (const ev of s) {}
const final = await s.result();
console.log("FINAL:", JSON.stringify(final));

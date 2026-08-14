#!/usr/bin/env node
// Capture authentic AssistantMessageEvent sequences from Pi's own faux provider.
//
// Prints one JSON.stringify(event) per line to STDOUT. The committed
// tests/fixtures/pi/events.corpus.jsonl is a FROZEN capture from this script — it is not
// auto-regenerated (faux uses non-deterministic ids/timestamps), so refresh manually:
//
//   node scripts/gen-event-corpus.mjs > tests/fixtures/pi/events.corpus.jsonl
//
// Requires the sibling Pi repo (../pi) with deps installed
// (cd ../pi && npm install --ignore-scripts) and the model catalog generated.
// Uses Pi's real streaming code (packages/ai/src/compat.ts + providers/faux.ts).

import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const compatPath = join(root, "..", "pi", "packages", "ai", "src", "compat.ts");
if (!existsSync(compatPath)) {
  console.error(`Pi source not found at ${compatPath}; cannot capture events.`);
  process.exit(1);
}

const { registerFauxProvider, stream, fauxAssistantMessage, fauxText, fauxThinking, fauxToolCall } =
  await import(pathToFileURL(compatPath).href);

async function collect(responses) {
  const reg = registerFauxProvider({ tokenSize: { min: 3, max: 3 } }); // deterministic chunking
  reg.setResponses(responses);
  const events = [];
  for await (const e of stream(reg.getModel(), {
    messages: [{ role: "user", content: "hi", timestamp: 1 }],
  })) {
    events.push(e);
  }
  return events;
}

const scenarios = [
  [
    fauxAssistantMessage(
      [
        fauxThinking("let me think about it"),
        fauxToolCall("bash", { command: "ls -la" }, { id: "call_1" }),
        fauxText("done now"),
      ],
      { stopReason: "toolUse" },
    ),
  ],
  [fauxAssistantMessage([fauxText("just some text output here")], { stopReason: "stop" })],
  [fauxAssistantMessage([fauxText("partial")], { stopReason: "aborted", errorMessage: "Request was aborted." })],
];

for (const responses of scenarios) {
  for (const e of await collect(responses)) {
    process.stdout.write(JSON.stringify(e) + "\n");
  }
}

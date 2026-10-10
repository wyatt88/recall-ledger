// Host API fixture plus the actual installed JS -> Python -> Rust IPC chain.
// This deliberately does not claim to run OpenCode itself.
import { pathToFileURL } from "node:url";
import { join } from "node:path";
let raw = "";
for await (const chunk of process.stdin) raw += chunk;
const input = JSON.parse(raw);
const { RecallLedgerPlugin } = await import(pathToFileURL(join(input.project, ".opencode/plugins/recall-ledger.js")));
const messages = [
  { info: { id: "u1", sessionID: input.session, role: "user" }, parts: [{ type: "text", text: input.body }] },
  { info: { id: "a1", parentID: "u1", sessionID: input.session, role: "assistant", time: { completed: 1 }, finish: "stop" },
    parts: [{ type: "text", text: "Assistant unverified proposal" }] },
];
const hooks = await RecallLedgerPlugin({
  directory: input.project,
  client: {
    session: {
      get: async () => ({ data: { id: input.session, directory: input.project } }),
      messages: async () => ({ data: messages }),
    },
    app: { log: async () => {} },
  },
});
const output = { system: ["Host controlled policy"] };
if (input.action === "capture") {
  await hooks["chat.message"]({ sessionID: input.session }, { message: messages[0].info, parts: messages[0].parts });
  await hooks.event({ event: { type: "session.idle", properties: { sessionID: input.session } } });
}
await hooks["experimental.chat.system.transform"]({ sessionID: input.session }, output);
await hooks.dispose();
process.stdout.write(JSON.stringify(output));

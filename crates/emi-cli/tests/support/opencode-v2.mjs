// Execute the installed V2 adapter against its public context interface,
// forwarding real subprocess checks and scripting the proposer, not the verdict.
import assert from "node:assert/strict"
import { readFile, writeFile } from "node:fs/promises"
import { setImmediate } from "node:timers/promises"

const [pluginPath, directory] = process.argv.slice(2)
const plugin = (await import(`data:text/javascript;base64,${Buffer.from(await readFile(pluginPath)).toString("base64")}`)).default
assert.equal(plugin.id, "emi-hook")
const prompts = []
const notes = []
let hook
let wake
let delivered
const events = []
const timeout = setTimeout(() => { throw new Error("V2 adapter did not complete") }, 60000)
const ctx = {
  location: { directory },
  session: {
    async get({ sessionID }) {
      return { id: sessionID, location: { directory }, outcome: "succeeded" }
    },
    async hook(name, fn) { assert.equal(name, "prompt"); hook = fn },
    async prompt({ sessionID, text, delivery, resume }) {
      assert.ok(text.startsWith("[emi]"))
      await hook({ sessionID, prompt: { text } })
      if (resume === false) {
        assert.equal(delivery, "queue")
        notes.push(text)
      } else {
        prompts.push(text)
      }
      delivered(text)
    },
  },
  event: {
    async *subscribe({ signal }) {
      signal.addEventListener("abort", () => wake?.(), { once: true })
      while (!signal.aborted) {
        if (!events.length) await new Promise((resolve) => { wake = resolve })
        if (events.length) yield events.shift()
      }
    },
  },
}
const cleanup = await plugin.setup(ctx)
async function idle() {
  const result = new Promise((resolve) => { delivered = resolve })
  // Native 2.0.18 execution events have no location field.
  events.push({ type: "session.execution.succeeded", data: { sessionID: "v2-loop" } })
  wake?.()
  const text = await result
  await setImmediate() // let the callback release its re-entrancy guard
  return text
}
try {
  await hook({ sessionID: "v2-loop", prompt: { text: "Handle None in add" } })
  await writeFile(`${directory}/app.py`, "def add(a, b):\n    return a - b\n")
  assert.match(await idle(), /repair round 1 of 3/)
  await writeFile(`${directory}/app.py`, "def add(a, b):\n    if a is None:\n        return b\n    return a + b\n")
  await writeFile(`${directory}/tests/test_new.py`, "from app import add\n\ndef test_none():\n    assert add(None, 2) == 2\n\ndef test_again():\n    assert add(1, 2) == 3\n")
  assert.match(await idle(), /repair round 2 of 3/)
  await writeFile(`${directory}/tests/test_new.py`, "from app import add\n\ndef test_none():\n    assert add(None, 2) == 2\n")
  assert.match(await idle(), /exercised/)
  assert.equal(prompts.length, 2)
  assert.equal(notes.length, 1)
  console.log("Installed V2 adapter: real checks, two repairs, one receipt; budget was not reset by repair prompts")
} finally {
  cleanup()
  clearTimeout(timeout)
}

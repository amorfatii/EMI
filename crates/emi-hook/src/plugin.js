// emi-hook: opencode plugin written by `emi hook install --harness opencode`.
// Remove it with `emi hook uninstall --harness opencode`. Edits are lost on reinstall.
//
// Every decision is made by `emi hook opencode`. This file only forwards two
// events to it and applies its answer:
//   chat.message  -> `emi hook opencode user-message`  (resets the repair rounds)
//   session.idle  -> `emi hook opencode idle`          (the agent finished a turn)
// opencode cannot block a stop, so a "repair" answer is sent back to the
// session as a follow-up prompt, which starts a new turn.
import { spawn } from "node:child_process"

const EMI = __EMI_COMMAND__
// A backstop only: `emi done` stops itself earlier and reports why.
const BACKSTOP_MS = 20 * 60 * 1000

const unchecked = (why) => ({
  action: "notify",
  tone: "attention",
  message: `EMI INCOMPLETE: this change was not checked.\nthe emi hook could not run: ${why}`,
  note: null,
})

function ask(event, payload) {
  return new Promise((resolve) => {
    let child
    try {
      child = spawn(EMI[0], [...EMI.slice(1), "hook", "opencode", event], {
        cwd: payload.directory,
        stdio: ["pipe", "pipe", "pipe"],
      })
    } catch (e) {
      resolve(unchecked(String(e)))
      return
    }
    let out = ""
    let err = ""
    const timer = setTimeout(() => child.kill("SIGTERM"), BACKSTOP_MS)
    child.stdout.on("data", (d) => (out += d))
    child.stderr.on("data", (d) => (err += d))
    child.on("error", (e) => {
      clearTimeout(timer)
      resolve(unchecked(e.message))
    })
    child.on("close", (code) => {
      clearTimeout(timer)
      if (code !== 0) {
        resolve(unchecked(`exit ${code}: ${err.trim().split("\n")[0]}`))
        return
      }
      try {
        resolve(JSON.parse(out))
      } catch {
        resolve(unchecked("its answer was not JSON"))
      }
    })
    child.stdin.on("error", () => {})
    child.stdin.end(JSON.stringify(payload))
  })
}

export const EmiHook = async ({ client, directory }) => {
  const checking = new Set()

  const tell = async (answer) => {
    const text = String(answer.message ?? "").slice(0, 1500)
    await client.tui.showToast({
      body: {
        message: text,
        variant: answer.tone === "verified" ? "success" : "warning",
        duration: 15000,
      },
    })
    if (answer.note) {
      // kept in the transcript for the person; noReply starts no turn
      await client.session.prompt({
        path: { id: answer.session },
        body: { noReply: true, parts: [{ type: "text", text: answer.note }] },
      })
    }
  }

  const apply = async (sessionID, answer) => {
    if (answer.action === "repair") {
      // not awaited: the call returns when the whole reply is finished
      client.session
        .prompt({
          path: { id: sessionID },
          body: { parts: [{ type: "text", text: String(answer.prompt) }] },
        })
        .catch((e) => tell({ ...unchecked(String(e)), session: sessionID }))
    } else if (answer.action === "notify") {
      await tell({ ...answer, session: sessionID })
    }
  }

  const isChild = async (sessionID) => {
    // when the lookup fails the session is treated as a root one: a check
    // too many is better than a skipped one
    try {
      const res = await client.session.get({ path: { id: sessionID } })
      return Boolean(res.data?.parentID)
    } catch {
      return false
    }
  }

  return {
    "chat.message": async (input, output) => {
      const text = output.parts
        .filter((p) => p.type === "text")
        .map((p) => p.text)
        .join("\n")
      const answer = await ask("user-message", { session_id: input.sessionID, directory, text })
      if (answer.action === "notify") await apply(input.sessionID, answer)
    },
    event: async ({ event }) => {
      if (event.type !== "session.idle") return
      const sessionID = event.properties.sessionID
      if (checking.has(sessionID)) return
      checking.add(sessionID)
      try {
        if (await isChild(sessionID)) return
        await apply(sessionID, await ask("idle", { session_id: sessionID, directory }))
      } finally {
        checking.delete(sessionID)
      }
    },
  }
}

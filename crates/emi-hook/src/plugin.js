// emi-hook: opencode plugin written by `emi hook install --harness opencode`.
// Remove it with `emi hook uninstall --harness opencode`. Edits are lost on reinstall.
//
// Every decision is made by `emi hook opencode`. This file only forwards two
// events to it and applies its answer:
//   prompt hook   -> `emi hook opencode user-message`  (resets the repair rounds)
//   execution succeeded -> `emi hook opencode idle`   (the agent finished a turn)
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

function ask(event, payload, signal) {
  return new Promise((resolve) => {
    let child
    try {
      child = spawn(EMI[0], [...EMI.slice(1), "hook", "opencode", event], {
        cwd: payload.directory,
        stdio: ["pipe", "pipe", "pipe"],
        detached: true,
      })
    } catch (e) {
      resolve(unchecked(String(e)))
      return
    }
    let out = ""
    let err = ""
    const kill = () => {
      try { process.kill(-child.pid, "SIGKILL") } catch {}
    }
    const timer = setTimeout(kill, BACKSTOP_MS)
    signal.addEventListener("abort", kill, { once: true })
    if (signal.aborted) kill()
    child.stdout.on("data", (d) => (out += d))
    child.stderr.on("data", (d) => (err += d))
    child.on("error", (e) => {
      clearTimeout(timer)
      signal.removeEventListener("abort", kill)
      resolve(unchecked(e.message))
    })
    child.on("close", (code) => {
      clearTimeout(timer)
      signal.removeEventListener("abort", kill)
      kill()
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

// This is a native OpenCode V2 definition. No runtime SDK import is needed:
// Plugin.define is an identity helper; the loader accepts this same shape.
export default {
  id: "emi-hook",
  async setup(ctx) {
    const controller = new AbortController()
    const checking = new Set()
    const directory = ctx.location.directory

    const localRoot = async (sessionID) => {
      const session = await ctx.session.get({ sessionID })
      return !session.parentID && session.location.directory === directory
    }
    const tell = async (sessionID, answer) => {
      // A non-resuming synthetic message stays hidden in the inbox until
      // another turn. Queue a marked receipt instead: V2 surfaces queued
      // prompts to the person, without spending another model call.
      await ctx.session.prompt({
        sessionID, text: answer.note || `[emi]\n${answer.message}`,
        delivery: "queue", resume: false,
      })
    }
    const apply = async (sessionID, answer) => {
      if (answer.action === "repair") {
        await ctx.session.prompt({ sessionID, text: String(answer.prompt) })
      } else if (answer.action === "notify") {
        await tell(sessionID, answer)
      } else if (answer.action !== "none") {
        await tell(sessionID, unchecked("unknown hook action"))
      }
    }

    await ctx.session.hook("prompt", async (event) => {
      if (!await localRoot(event.sessionID)) return
      const answer = await ask("user-message", {
        session_id: event.sessionID, directory, text: event.prompt.text,
      }, controller.signal)
      await apply(event.sessionID, answer)
    })

    const idle = async (sessionID) => {
      if (checking.has(sessionID)) return
      checking.add(sessionID)
      try {
        if (!await localRoot(sessionID)) return
        const session = await ctx.session.get({ sessionID })
        // An interrupt or provider error is not a completion claim.
        if (session.outcome !== "succeeded") return
        await apply(sessionID, await ask("idle", {
          session_id: sessionID, directory,
        }, controller.signal))
      } catch (e) {
        await tell(sessionID, unchecked(String(e)))
      } finally {
        checking.delete(sessionID)
      }
    }
    void (async () => {
      for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
        if (event.type !== "session.execution.succeeded" && event.type !== "session.idle") continue
        // Execution events in V2 can omit location. The session lookup
        // above still checks location and parentage before launching EMI.
        if (event.location && event.location.directory !== directory) continue
        void idle(event.data.sessionID).catch(console.error)
      }
    })().catch((e) => {
      if (!controller.signal.aborted) console.error("EMI event subscription failed", e)
    })
    return () => controller.abort()
  },
}

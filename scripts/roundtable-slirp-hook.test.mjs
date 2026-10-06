import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import fs from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'

const source = new URL('../src-tauri/src/roundtable/sandbox/linux_oci.rs', import.meta.url)

async function waitUntil(check) {
  const deadline = Date.now() + 5000
  while (Date.now() < deadline) {
    if (await check()) return
    await new Promise((resolve) => setTimeout(resolve, 20))
  }
  assert.fail('fake helper did not stop within the cleanup bound')
}

async function running(pid) {
  try {
    const stat = await fs.readFile(`/proc/${pid}/stat`, 'utf8')
    return stat.slice(stat.lastIndexOf(') ') + 2).split(' ')[0] !== 'Z'
  } catch (error) {
    if (error.code === 'ENOENT' || error.code === 'ESRCH') return false
    throw error
  }
}

test('slirp hook binds ownership and exits via its own pipe when the container ends', {
  skip: process.platform !== 'linux',
  timeout: 15000,
}, async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'rt-fake-slirp-'))
  const container = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], {
    stdio: 'ignore',
  })
  const containerExit = once(container, 'exit')
  let helperPid
  let watcherPid
  try {
    const rust = await fs.readFile(source, 'utf8')
    const hook = rust.split('pub(crate) const SLIRP_HOOK_SCRIPT: &str = r#"')[1].split('"#;')[0]
    const hookPath = path.join(root, 'hook.sh')
    const helperPath = path.join(root, 'fake-slirp')
    const pidfile = path.join(root, 'owned.pid')
    await fs.writeFile(`${pidfile}.lock`, '', { mode: 0o600 })
    await fs.writeFile(`${pidfile}.state`, 'prepared\n', { mode: 0o600 })
    await fs.writeFile(hookPath, hook, { mode: 0o700 })
    await fs.writeFile(helperPath, `#!${process.execPath}\nconst fs = require('node:fs');\nif (!process.argv.includes('--exit-fd=0')) process.exit(7);\nfs.writeSync(3, '1'); fs.closeSync(3);\nprocess.stdin.resume(); process.stdin.on('end', () => process.exit(0));\n`, { mode: 0o700 })
    const child = spawn('sh', [hookPath, helperPath, pidfile, 'fake-owned-container', root], {
      stdio: ['pipe', 'ignore', 'ignore'],
      env: { ...process.env, CODEG_ROUNDTABLE_SLIRP_OWNER: 'fake-owned-container', CODEG_ROUNDTABLE_SLIRP_ROOT: root, CODEG_ROUNDTABLE_SLIRP_ROLE: 'hook' },
    })
    const hookExit = once(child, 'exit')
    child.stdin.end(JSON.stringify({ pid: container.pid }))
    const [code] = await hookExit
    assert.equal(code, 0, 'the hook must provide a lifecycle exit fd to the fake helper')
    const fields = (await fs.readFile(pidfile, 'utf8')).trim().split(/\s+/)
    assert.equal(fields.length, 6, 'both helper and watcher need ownership pins')
    assert.equal(fields[0], 'slirp')
    assert.equal(fields[3], 'watcher')
    helperPid = Number(fields[1])
    watcherPid = Number(fields[4])
    assert.ok(Number(fields[2]) > 0)
    assert.ok(Number(fields[5]) > 0)
    assert.ok(await running(watcherPid))
    assert.ok(await running(helperPid))
    const env = await fs.readFile(`/proc/${helperPid}/environ`, 'utf8')
    assert.ok(env.split('\0').includes('CODEG_ROUNDTABLE_SLIRP_OWNER=fake-owned-container'))
    assert.ok(env.split('\0').includes(`CODEG_ROUNDTABLE_SLIRP_ROOT=${root}`))
    const watcherEnv = await fs.readFile(`/proc/${watcherPid}/environ`, 'utf8')
    assert.ok(watcherEnv.split('\0').includes('CODEG_ROUNDTABLE_SLIRP_ROLE=watcher'))
    assert.ok(watcherEnv.split('\0').includes('CODEG_ROUNDTABLE_SLIRP_OWNER=fake-owned-container'))
    await waitUntil(async () => {
      for (const pid of (await fs.readdir('/proc')).filter((entry) => /^\d+$/.test(entry))) {
        const stat = await fs.readFile(`/proc/${pid}/stat`, 'utf8').catch(() => '')
        if (Number(stat.slice(stat.lastIndexOf(') ') + 2).split(' ')[1]) !== watcherPid) continue
        const inherited = await fs.readFile(`/proc/${pid}/environ`, 'utf8').catch(() => '')
        if (inherited.split('\0').includes('CODEG_ROUNDTABLE_SLIRP_ROLE=watcher')) return true
      }
      return false
    })
    container.kill('SIGKILL')
    await containerExit
    await waitUntil(async () => !(await running(helperPid)) && !(await running(watcherPid)))
  } finally {
    if (container.exitCode === null && container.signalCode === null) container.kill('SIGKILL')
    await containerExit
    if (helperPid) await waitUntil(async () => !(await running(helperPid)))
    if (watcherPid) await waitUntil(async () => !(await running(watcherPid)))
    await fs.rm(root, { recursive: true, force: true })
  }
})

test('slirp hook cannot succeed when its ownership pin cannot be written', {
  skip: process.platform !== 'linux',
  timeout: 15000,
}, async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'rt-fake-slirp-pin-'))
  const container = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' })
  const containerExit = once(container, 'exit')
  try {
    const rust = await fs.readFile(source, 'utf8')
    const hook = rust.split('pub(crate) const SLIRP_HOOK_SCRIPT: &str = r#"')[1].split('"#;')[0]
    const hookPath = path.join(root, 'hook.sh')
    const helperPath = path.join(root, 'fake-slirp')
    const pin = path.join(root, 'unwritable-pin')
    await fs.mkdir(pin)
    await fs.writeFile(`${pin}.lock`, '', { mode: 0o600 })
    await fs.writeFile(`${pin}.state`, 'prepared\n', { mode: 0o600 })
    await fs.writeFile(hookPath, hook, { mode: 0o700 })
    await fs.writeFile(helperPath, `#!${process.execPath}\nconst fs = require('node:fs'); fs.writeSync(3, '1'); fs.closeSync(3); process.stdin.resume(); process.stdin.on('end', () => process.exit(0));\n`, { mode: 0o700 })
    const child = spawn('sh', [hookPath, helperPath, pin, 'fake-pin-failure', root], {
      stdio: ['pipe', 'ignore', 'ignore'],
      env: { ...process.env, CODEG_ROUNDTABLE_SLIRP_OWNER: 'fake-pin-failure', CODEG_ROUNDTABLE_SLIRP_ROOT: root, CODEG_ROUNDTABLE_SLIRP_ROLE: 'hook' },
    })
    const hookExit = once(child, 'exit')
    child.stdin.end(JSON.stringify({ pid: container.pid }))
    const [code] = await hookExit
    assert.notEqual(code, 0, 'missing ownership pin cannot become successful startup')
  } finally {
    container.kill('SIGKILL')
    await containerExit
    // The fake helper receives EOF after the watcher observes container exit.
    await new Promise((resolve) => setTimeout(resolve, 300))
    await fs.rm(root, { recursive: true, force: true })
  }
})

test('cleanup gate waits for pin publication and prevents a late hook from spawning', {
  skip: process.platform !== 'linux',
  timeout: 15000,
}, async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'rt-fake-slirp-race-'))
  const container = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' })
  const containerExit = once(container, 'exit')
  const owner = 'fake-publish-race'
  const pin = path.join(root, 'owned.pid')
  const entered = path.join(root, 'before-pin')
  const release = path.join(root, 'release-pin')
  const cleaned = path.join(root, 'startup-cancelled')
  const starts = path.join(root, 'helper-starts')
  const pids = []
  try {
    const rust = await fs.readFile(source, 'utf8')
    const hookPath = path.join(root, 'hook.sh')
    const helperPath = path.join(root, 'fake-slirp')
    const bin = path.join(root, 'bin')
    await fs.mkdir(bin)
    await fs.writeFile(`${pin}.lock`, '', { mode: 0o600 })
    await fs.writeFile(`${pin}.state`, 'prepared\n', { mode: 0o600 })
    await fs.writeFile(hookPath, rust.split('pub(crate) const SLIRP_HOOK_SCRIPT: &str = r#"')[1].split('"#;')[0], { mode: 0o700 })
    await fs.writeFile(helperPath, `#!${process.execPath}\nconst fs = require('node:fs'); fs.appendFileSync(${JSON.stringify(starts)}, process.pid + '\\n'); fs.writeSync(3, '1'); fs.closeSync(3); process.stdin.resume(); process.stdin.on('end', () => process.exit(0));\n`, { mode: 0o700 })
    // Pause the real hook's second start-time read, after both children fork
    // but before either pin is published. The watcher uses another role.
    await fs.writeFile(path.join(bin, 'cut'), `#!/bin/sh\nif [ "$CODEG_ROUNDTABLE_SLIRP_ROLE" = hook ]; then\n if [ -f '${root}/first-cut' ]; then\n  touch '${entered}'\n  while [ ! -f '${release}' ]; do sleep 0.02; done\n else touch '${root}/first-cut'; fi\nfi\nexec /usr/bin/cut "$@"\n`, { mode: 0o700 })
    const env = { ...process.env, PATH: `${bin}:${process.env.PATH}`, CODEG_ROUNDTABLE_SLIRP_OWNER: owner, CODEG_ROUNDTABLE_SLIRP_ROOT: root, CODEG_ROUNDTABLE_SLIRP_ROLE: 'hook' }
    const launch = () => {
      const child = spawn('sh', [hookPath, helperPath, pin, owner, root], { stdio: ['pipe', 'ignore', 'ignore'], env })
      const exit = once(child, 'exit')
      child.stdin.end(JSON.stringify({ pid: container.pid }))
      return exit
    }
    const hookExit = launch()
    await waitUntil(async () => fs.access(entered).then(() => true, () => false))
    const cleanup = spawn('flock', ['-x', `${pin}.lock`, 'sh', '-c', `printf 'cleanup-in-progress-started\\n' > '${pin}.state'; sync -f '${pin}.state'; touch '${cleaned}'`], { stdio: 'ignore' })
    const cleanupExit = once(cleanup, 'exit')
    await new Promise((resolve) => setTimeout(resolve, 80))
    assert.equal(await fs.access(cleaned).then(() => true, () => false), false, 'cleanup must not pass the in-flight startup gate')
    await fs.writeFile(release, '')
    assert.equal((await hookExit)[0], 0)
    assert.equal((await cleanupExit)[0], 0)
    const entries = (await fs.readFile(pin, 'utf8')).trim().split('\n').map((line) => line.split(/\s+/))
    pids.push(...entries.map((entry) => Number(entry[1])))
    assert.notEqual((await launch())[0], 0, 'cancelled startup must reject a late hook')
    assert.equal((await fs.readFile(starts, 'utf8')).trim().split('\n').length, 1)
  } finally {
    await fs.writeFile(release, '').catch(() => {})
    container.kill('SIGKILL')
    await containerExit
    for (const pid of pids) await waitUntil(async () => !(await running(pid)))
    await new Promise((resolve) => setTimeout(resolve, 250))
    await fs.rm(root, { recursive: true, force: true })
  }
})

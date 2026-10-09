// Installs the Windows build the way a user would and drives the real app:
// first launch, the Start screen, the demo project in its own window, and
// HTTP and HTTPS traffic captured through the project's proxy.
//
//   node windows-smoke.mjs <Plonix-Windows-setup.exe> <output folder>
//
// Screenshots of each step land in the output folder. The app stays
// installed, for the command check that follows; uninstall.exe /S removes it.

import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import tls from 'node:tls';
import { chromium } from 'playwright-core';

const [setup, out] = process.argv.slice(2).map((p) => path.resolve(p));
const home = path.join(out, 'home');
fs.mkdirSync(home, { recursive: true });
const CDP = 9333;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const step = (s) => console.log(`\n== ${s}`);

async function until(what, fn, ms = 60000) {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    try {
      const v = await fn();
      if (v) return v;
    } catch (e) {
      last = e;
    }
    await sleep(250);
  }
  throw new Error(`timed out waiting for ${what}${last ? `: ${last.message}` : ''}`);
}

// A screenshot of the whole screen; `print` also puts a small copy in the log.
function desktop(name, print = false) {
  const file = path.join(out, name);
  const small = path.join(out, 'small.jpg');
  const q = (p) => p.replace(/'/g, "''");
  const ps = `Add-Type -AssemblyName System.Windows.Forms,System.Drawing
$b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
[System.Drawing.Graphics]::FromImage($bmp).CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
$bmp.Save('${q(file)}', [System.Drawing.Imaging.ImageFormat]::Png)
$s = New-Object System.Drawing.Bitmap $bmp, ([int]($b.Width / 2)), ([int]($b.Height / 2))
$s.Save('${q(small)}', [System.Drawing.Imaging.ImageFormat]::Jpeg)`;
  execFileSync('powershell', ['-NoProfile', '-Command', ps], { stdio: 'inherit' });
  if (print) printImage(name, fs.readFileSync(small));
  fs.rmSync(small, { force: true });
}

function printImage(name, bytes) {
  console.log(`--- ${name} jpeg base64 ---\n${bytes.toString('base64').match(/.{1,4000}/g).join('\n')}\n--- end ${name} ---`);
}

async function shot(page, name) {
  await page.screenshot({ path: path.join(out, name) });
  printImage(name, await page.screenshot({ type: 'jpeg', quality: 45 }));
}

step('Install silently');
execFileSync(setup, ['/S'], { stdio: 'inherit' });
const dir = path.join(process.env.LOCALAPPDATA, 'Plonix');
const files = await until('the installed files', () => fs.existsSync(path.join(dir, 'Plonix.exe')) && fs.readdirSync(dir));
console.log(`${dir}: ${files.join(', ')}`);
const cli = files.find((f) => /^plonix-cli.*\.exe$/i.test(f));
if (!cli) throw new Error('the plonix command was not installed next to the app');
console.log(execFileSync(path.join(dir, cli), ['--version'], { encoding: 'utf8' }).trim());

step('Launch Plonix');
const env = { ...process.env, PLONIX_HOME: home, PLONIX_NO_ANALYTICS: '1', PLONIX_WEBVIEW_DEBUG_PORT: String(CDP) };
delete env.PLONIX_ACCEPT_TERMS;
const app = spawn(path.join(dir, 'Plonix.exe'), [], { env, stdio: ['ignore', 'pipe', 'pipe'] });
app.stdout.on('data', (d) => process.stdout.write(`[app] ${d}`));
app.stderr.on('data', (d) => process.stdout.write(`[app] ${d}`));
let exited = null;
app.on('exit', (code) => (exited = code));

let failed = null;
try {
  await until('the app window', async () => {
    if (exited !== null) throw new Error(`Plonix exited with code ${exited}`);
    return (await fetch(`http://127.0.0.1:${CDP}/json/version`)).ok;
  }, 90000);
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${CDP}`);
  const pages = () => browser.contexts().flatMap((c) => c.pages());
  const launcher = await until('the Start screen', () => pages().find((p) => /^http:\/\/(127\.0\.0\.1|localhost)[:/]/.test(p.url())));

  step('First launch: license and terms');
  await launcher.waitForSelector('#t-accept', { timeout: 60000 });
  await shot(launcher, '1-welcome.png');
  desktop('0-desktop.png');
  await launcher.check('#t-accept');
  await launcher.getByRole('button', { name: 'Continue' }).click();

  step('Start screen');
  const demo = launcher.getByRole('button', { name: 'Try the Demo' }).first();
  await demo.waitFor({ timeout: 30000 });
  await shot(launcher, '2-start.png');

  step('Open the demo project in its own window');
  await demo.click();
  const hubUrl = new URL(launcher.url()).origin;
  const project = await until('the project window', () => pages().find((p) => p !== launcher && /^http:\/\/(127\.0\.0\.1|localhost)[:/]/.test(p.url()) && new URL(p.url()).origin !== hubUrl));
  await project.waitForLoadState();
  await until('demo traffic in the window', () => project.evaluate(() => document.body.innerText.includes('brightcart')));
  await project.evaluate(() => window.endTour?.());
  await shot(project, '3-demo.png');

  step('Capture traffic through the proxy');
  const token = fs.readFileSync(path.join(home, 'api-token'), 'utf8').trim();
  const hub = JSON.parse(fs.readFileSync(path.join(home, 'hub.json'), 'utf8'));
  const call = async (base, p, method = 'GET') => {
    const r = await fetch(base + p, { method, headers: { Authorization: `Bearer ${token}`, 'content-type': 'application/json' }, body: method === 'POST' ? '{}' : undefined });
    if (!r.ok) throw new Error(`${method} ${p}: ${r.status} ${await r.text()}`);
    return r.json();
  };
  const listed = await call(hub.url.replace(/\/$/, ''), '/api/projects');
  const open = (listed.projects ?? listed).find((p) => p.session);
  if (!open) throw new Error(`no open project: ${JSON.stringify(listed)}`);
  const info = { proxy: open.session.proxy, api: /^http/.test(open.session.api) ? open.session.api : `http://${open.session.api}` };
  console.log(`proxy ${info.proxy}, api ${info.api}`);
  const [proxyHost, proxyPort] = info.proxy.replace(/^https?:\/\//, '').split(':');

  const server = http.createServer((_, res) => res.end('hello from the smoke test'));
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const plain = await new Promise((resolve, reject) => {
    const url = `http://localhost:${server.address().port}/smoke-plain`;
    http.get({ host: proxyHost, port: proxyPort, path: url, headers: { host: `localhost:${server.address().port}` } }, (res) => {
      let body = '';
      res.on('data', (d) => (body += d)).on('end', () => resolve(body));
    }).on('error', reject);
  });
  if (!plain.includes('hello from the smoke test')) throw new Error(`HTTP through the proxy returned: ${plain}`);
  console.log('HTTP through the proxy: ok');

  // HTTPS: a CONNECT tunnel, verified against the project's own CA only.
  const ca = fs.readFileSync(path.join(home, 'ca.pem'));
  const secure = await new Promise((resolve, reject) => {
    const req = http.request({ host: proxyHost, port: proxyPort, method: 'CONNECT', path: 'example.com:443' });
    req.on('connect', (_, socket) => {
      const s = tls.connect({ socket, servername: 'example.com', ca }, () => {
        s.write('GET /smoke-tls HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n');
      });
      let body = '';
      s.on('data', (d) => (body += d)).on('end', () => resolve(body)).on('error', reject);
    });
    req.on('error', reject);
    req.end();
  });
  console.log(`HTTPS through the proxy: ${secure.split('\r\n')[0]}`);
  server.close();

  const seen = await until('both requests in Traffic', async () => {
    const t = await call(info.api.replace(/\/$/, ''), '/api/traffic?limit=500');
    const paths = t.items.map((e) => `${e.host}${e.path}`);
    return paths.some((p) => p.endsWith('/smoke-plain')) && paths.some((p) => p === 'example.com/smoke-tls') && paths;
  }, 20000);
  console.log(`captured: ${seen.filter((p) => p.includes('smoke')).join(', ')}`);
  await project.evaluate(() => window.go?.('traffic'));
  await sleep(1500);
  await shot(project, '4-traffic.png');
  desktop('5-desktop.png');
  await browser.close().catch(() => {});
} catch (e) {
  failed = e;
  console.log(`--- Plonix home ---\n${fs.readdirSync(home).join(', ')}`);
  for (const [what, cmd, args] of [
    ['Processes', 'tasklist', []],
    ['Listening ports', 'netstat', ['-ano', '-p', 'TCP']],
  ]) {
    try {
      console.log(`--- ${what} ---\n${execFileSync(cmd, args, { encoding: 'utf8' })}`);
    } catch {}
  }
  try {
    desktop('failure-desktop.png', true);
  } catch {}
} finally {
  if (exited === null) execFileSync('taskkill', ['/PID', String(app.pid), '/T', '/F'], { stdio: 'ignore' });
}

if (failed) {
  console.error(failed);
  process.exit(1);
}
console.log('\nThe Windows app works.');

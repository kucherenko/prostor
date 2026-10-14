// End-to-end smoke test: opens the running app in headless Chrome and checks that
// the Svelte page rendered the scene returned by the Rust core through WASM.
//
//   PROSTOR_URL=http://127.0.0.1:8080/ pnpm smoke
//   PROSTOR_CHROME=/path/to/chrome   (optional; defaults to the installed Google Chrome)

import puppeteer from 'puppeteer-core';

const url = process.env.PROSTOR_URL ?? 'http://localhost:8080/';
const timeout = 20_000;

// Chrome asks for /favicon.ico before the page sets its icon; a 404 there is noise.
const ignoredPaths = ['/favicon.ico'];

const launchOptions = process.env.PROSTOR_CHROME
	? { executablePath: process.env.PROSTOR_CHROME }
	: { channel: 'chrome' };

const browser = await puppeteer.launch({
	...launchOptions,
	headless: true,
	args: ['--no-sandbox', '--disable-gpu']
});

const problems = [];
try {
	const page = await browser.newPage();
	page.on('pageerror', (err) => problems.push(`pageerror: ${err.message}`));
	page.on('console', (msg) => {
		if (msg.type() !== 'error') return;
		const where = msg.location()?.url;
		if (where && ignoredPaths.some((p) => new URL(where).pathname === p)) return;
		problems.push(`console.error: ${msg.text()}${where ? ` (${where})` : ''}`);
	});
	page.on('response', (response) => {
		const { pathname } = new URL(response.url());
		if (response.status() >= 400 && !ignoredPaths.includes(pathname)) {
			problems.push(`HTTP ${response.status()} for ${response.url()}`);
		}
	});

	await page.goto(url, { waitUntil: 'networkidle0', timeout });
	await page.waitForSelector('[data-testid="scene-name"], [data-testid="error"]', { timeout });

	const result = await page.evaluate(() => ({
		error: document.querySelector('[data-testid="error"]')?.textContent ?? null,
		sceneName: document.querySelector('[data-testid="scene-name"]')?.textContent ?? null,
		wasmVersion: document.querySelector('[data-testid="wasm-version"]')?.textContent ?? null,
		layers: document.querySelectorAll('[data-testid="layer"]').length
	}));

	if (result.error) problems.push(result.error);
	if (result.sceneName !== 'Hello from Rust')
		problems.push(`unexpected scene name: ${result.sceneName}`);
	if (result.layers !== 2) problems.push(`expected 2 layers, got ${result.layers}`);
	if (!result.wasmVersion) problems.push('wasm version missing');

	console.log(JSON.stringify({ ok: problems.length === 0, url, ...result, problems }, null, 2));
} finally {
	await browser.close();
}

process.exit(problems.length === 0 ? 0 : 1);

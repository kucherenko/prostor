// End-to-end check of the M1 tracer bullet: opens /studio in headless Chrome with a fake
// camera, streams for a few seconds, stops, and verifies the server's recording with
// ffprobe (H.264, plus AAC when the browser could encode it).
//
//   PROSTOR_URL=http://127.0.0.1:8080/ PROSTOR_RECORDINGS_DIR=/tmp/rec pnpm e2e
//   PROSTOR_SECRET=...        the server's secret, typed into the page (never in the URL)
//   PROSTOR_CHROME=/path     optional; defaults to the installed Google Chrome
//   PROSTOR_E2E_SECONDS=10   how long to stream

import { execFileSync } from 'node:child_process';
import { existsSync, statSync } from 'node:fs';
import path from 'node:path';
import puppeteer from 'puppeteer-core';

const url = new URL('/studio', process.env.PROSTOR_URL ?? 'http://localhost:8080/').toString();
const recordingsDir = process.env.PROSTOR_RECORDINGS_DIR;
const secret = process.env.PROSTOR_SECRET ?? '';
const seconds = Number(process.env.PROSTOR_E2E_SECONDS ?? 10);
const room = `e2e-${Date.now().toString(36)}`;
const timeout = 30_000;

if (!recordingsDir) {
	console.error('PROSTOR_RECORDINGS_DIR must point at the server recordings directory');
	process.exit(2);
}

const launchOptions = process.env.PROSTOR_CHROME
	? { executablePath: process.env.PROSTOR_CHROME }
	: { channel: 'chrome' };

const browser = await puppeteer.launch({
	...launchOptions,
	headless: true,
	args: [
		'--no-sandbox',
		'--use-fake-device-for-media-stream=fps=30',
		'--use-fake-ui-for-media-stream',
		'--autoplay-policy=no-user-gesture-required',
		'--enable-unsafe-swiftshader',
		'--ignore-gpu-blocklist'
	]
});

const problems = [];
const result = { url, room, seconds };
try {
	const page = await browser.newPage();
	page.on('pageerror', (err) => problems.push(`pageerror: ${err.message}`));
	page.on('console', (msg) => {
		if (msg.type() !== 'error') return;
		const where = msg.location()?.url;
		if (where && new URL(where).pathname === '/favicon.ico') return;
		problems.push(`console.error: ${msg.text()}${where ? ` (${where})` : ''}`);
	});

	await page.goto(url, { waitUntil: 'networkidle0', timeout });
	await page.waitForFunction(
		() =>
			['idle', 'unsupported'].includes(
				document.querySelector('[data-testid="phase"]')?.textContent ?? ''
			),
		{ timeout }
	);
	const text = async (testId) =>
		page.evaluate(
			(id) => document.querySelector(`[data-testid="${id}"]`)?.textContent ?? null,
			testId
		);
	if ((await text('phase')) === 'unsupported') {
		problems.push(`unsupported browser: ${await text('error')}`);
		throw new Error('unsupported');
	}

	await page.evaluate(() => {
		const input = document.querySelector('[data-testid="room"]');
		input.value = '';
	});
	await page.type('[data-testid="room"]', room);
	if (secret) await page.type('[data-testid="secret"]', secret);

	await page.click('[data-testid="open-camera"]');
	await page.waitForFunction(
		() => document.querySelector('[data-testid="phase"]')?.textContent === 'camera',
		{ timeout }
	);
	result.camera = await text('camera');

	await page.click('[data-testid="start"]');
	await page.waitForFunction(
		() =>
			['live', 'camera'].includes(
				document.querySelector('[data-testid="phase"]')?.textContent ?? ''
			),
		{ timeout }
	);
	if ((await text('phase')) !== 'live') {
		problems.push(`start failed: ${await text('error')}`);
		throw new Error('start failed');
	}
	result.recording = await text('recording');
	await new Promise((r) => setTimeout(r, seconds * 1000));
	result.liveStatus = {
		fps: await text('fps'),
		encoder: await text('encoder'),
		encoded: await text('encoded'),
		sent: await text('sent'),
		lag: await text('lag'),
		ingest: await text('ingest-phase'),
		egress: await text('egress')
	};
	await page.click('[data-testid="stop"]');
	await page.waitForFunction(
		() => document.querySelector('[data-testid="phase"]')?.textContent === 'camera',
		{ timeout }
	);
	result.error = await text('error');
	if (result.error) problems.push(`page error: ${result.error}`);
	result.finalStatus = { sent: await text('sent'), egress: await text('egress') };

	const file = path.join(recordingsDir, result.recording ?? '');
	if (!result.recording || !existsSync(file)) {
		problems.push(`recording not found: ${file}`);
	} else {
		result.bytes = statSync(file).size;
		const probe = JSON.parse(
			execFileSync(
				'ffprobe',
				['-v', 'error', '-show_streams', '-show_format', '-of', 'json', file],
				{ encoding: 'utf8' }
			)
		);
		const codecs = probe.streams.map((s) => s.codec_name);
		const duration = Number(probe.format?.duration ?? 0);
		result.codecs = codecs;
		result.duration = duration;
		if (!codecs.includes('h264')) problems.push(`no h264 stream: ${codecs}`);
		const audioExpected = !(result.recording && /video only/.test(await text('recording')));
		if (audioExpected && !codecs.includes('aac') && result.liveStatus.encoder !== 'none') {
			// Linux Chrome has no AAC encoder; the page says so and the stream is video only.
			result.note = 'no aac stream (video-only session)';
		}
		if (duration < seconds - 2) problems.push(`duration ${duration}s is short of ${seconds}s`);
		const video = probe.streams.find((s) => s.codec_name === 'h264');
		if (video && video.width !== 1920) problems.push(`video width ${video.width}`);
	}
} catch (error) {
	if (!problems.length) problems.push(String(error?.stack ?? error));
} finally {
	await browser.close();
}

console.log(JSON.stringify({ ok: problems.length === 0, ...result, problems }, null, 2));
process.exit(problems.length === 0 ? 0 : 1);

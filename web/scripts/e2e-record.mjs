// End-to-end check of the M1 tracer bullet: opens /studio in headless Chrome with a fake
// camera, passes the lobby (display name, room, secret, Join), streams for a few seconds,
// stops, and verifies the server's recording with ffprobe (H.264, plus AAC when the browser
// could encode it).
//
//   PROSTOR_URL=http://127.0.0.1:8080/ PROSTOR_RECORDINGS_DIR=/tmp/rec pnpm e2e
//   PROSTOR_SECRET=...        the server's secret, typed into the lobby (never in the URL)
//   PROSTOR_CHROME=/path     optional; defaults to the installed Google Chrome
//   PROSTOR_E2E_SECONDS=10   how long to stream
//   PROSTOR_E2E_RTMP_URL=rtmp://127.0.0.1:1936/live  PROSTOR_E2E_RTMP_KEY=e2e
//                            optional: declare this destination too (an ffmpeg listener)
//   PROSTOR_E2E_RTMP_OUT=/path/received.flv   optional: ffprobe what the listener wrote

import { execFileSync } from 'node:child_process';
import { existsSync, statSync } from 'node:fs';
import path from 'node:path';
import puppeteer from 'puppeteer-core';

const url = new URL('/studio', process.env.PROSTOR_URL ?? 'http://localhost:8080/').toString();
const recordingsDir = process.env.PROSTOR_RECORDINGS_DIR;
const secret = process.env.PROSTOR_SECRET ?? '';
const seconds = Number(process.env.PROSTOR_E2E_SECONDS ?? 10);
const rtmpUrl = process.env.PROSTOR_E2E_RTMP_URL ?? '';
const rtmpKey = process.env.PROSTOR_E2E_RTMP_KEY ?? '';
const rtmpOut = process.env.PROSTOR_E2E_RTMP_OUT ?? '';
const room = `e2e-${Date.now().toString(36)}`;
const displayName = 'E2E Producer';
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

/** A failure already recorded in `problems`; the catch below must not add it again. */
class Reported extends Error {}

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
			['lobby', 'unsupported'].includes(
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
		throw new Reported('unsupported');
	}

	// Lobby: the fake camera shows in the self-view; name, room and secret → Join.
	const fill = async (selector, value) => {
		await page.$eval(selector, (input) => {
			input.value = '';
		});
		await page.type(selector, value);
	};
	await fill('[data-testid="lobby-name"]', displayName);
	await fill('[data-testid="room"]', room);
	if (secret) await page.type('[data-testid="secret"]', secret);
	await page.waitForFunction(
		() => {
			const join = document.querySelector('[data-testid="lobby-join"]');
			const preview = document.querySelector('[data-testid="lobby-preview"]');
			return join && !join.disabled && preview && preview.videoWidth > 0;
		},
		{ timeout }
	);
	result.lobby = await page.evaluate(() => {
		// Only options with a value are devices: an empty one is the placeholder of an empty list.
		const devices = (testId) =>
			Array.from(document.querySelector(`[data-testid="${testId}"]`)?.options ?? []).filter(
				(option) => option.value !== ''
			).length;
		const video = document.querySelector('[data-testid="lobby-preview"]');
		return {
			cameras: devices('lobby-camera'),
			microphones: devices('lobby-microphone'),
			preview: `${video.videoWidth}×${video.videoHeight}`
		};
	});
	if (!result.lobby.cameras) problems.push('the lobby lists no camera');
	await page.click('[data-testid="lobby-join"]');
	// Join ends in `camera`, or back in the lobby with the reason in the form. The error element
	// exists only after a failed Join, which tells that apart from not having left the lobby yet.
	await page.waitForFunction(
		() => {
			const phase = document.querySelector('[data-testid="phase"]')?.textContent;
			return (
				phase === 'camera' ||
				(phase === 'lobby' && document.querySelector('[data-testid="error"]') !== null)
			);
		},
		{ timeout }
	);
	if ((await text('phase')) !== 'camera') {
		problems.push(`join failed: ${await text('error')}`);
		throw new Reported('join failed');
	}
	result.camera = await text('camera');
	if ((await text('joined-name')) !== displayName) {
		problems.push(`joined as ${await text('joined-name')}, expected ${displayName}`);
	}
	if ((await text('joined-room')) !== room) {
		problems.push(`joined room ${await text('joined-room')}, expected ${room}`);
	}
	// The lobby remembers the name; the producer secret must never reach localStorage.
	const stored = await page.evaluate(() =>
		Array.from({ length: localStorage.length }, (_, i) => {
			const key = localStorage.key(i);
			return [key, localStorage.getItem(key)];
		})
	);
	if (!stored.some(([key, value]) => key === 'prostor.lobby.name' && value === displayName)) {
		problems.push('the lobby did not remember the display name');
	}
	if (secret && stored.some(([key, value]) => `${key}${value}`.includes(secret))) {
		problems.push('the producer secret was written to localStorage');
	}

	// The studio shows after Join, so the destinations are filled in now.
	if (rtmpUrl) {
		await page.click('[data-testid="dest-add"]');
		await page.type('[data-testid="dest-name-0"]', 'e2e');
		await page.type('[data-testid="dest-url-0"]', rtmpUrl);
		await page.type('[data-testid="dest-key-0"]', rtmpKey);
	}

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
		throw new Reported('start failed');
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
	result.egress = await page.evaluate(() =>
		Array.from(document.querySelectorAll('[data-testid="egress"]')).map((e) => ({
			id: e.getAttribute('data-egress'),
			text: (e.textContent ?? '').replace(/\s+/g, ' ').trim()
		}))
	);
	if (rtmpUrl) {
		const rtmpLine = result.egress.find((e) => String(e.id).startsWith('rtmp-'));
		if (!rtmpLine) problems.push('no rtmp egress in the status');
		else if (!/^stopped /.test(rtmpLine.text ?? '')) problems.push(`rtmp egress: ${rtmpLine.text}`);
		else if (/ 0 tags/.test(rtmpLine.text ?? ''))
			problems.push(`rtmp egress sent nothing: ${rtmpLine.text}`);
	}

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
	if (rtmpOut) {
		// The listener (ffmpeg) finalises its file when the publisher sends deleteStream.
		let probed = null;
		for (let attempt = 0; attempt < 50 && !probed; attempt++) {
			try {
				probed = JSON.parse(
					execFileSync(
						'ffprobe',
						['-v', 'error', '-show_streams', '-show_format', '-of', 'json', rtmpOut],
						{ encoding: 'utf8' }
					)
				);
			} catch {
				await new Promise((r) => setTimeout(r, 200));
			}
		}
		if (!probed) problems.push(`rtmp listener output not probeable: ${rtmpOut}`);
		else {
			const codecs = probed.streams.map((s) => s.codec_name);
			const duration = Number(probed.format?.duration ?? 0);
			result.rtmp = { codecs, duration, bytes: statSync(rtmpOut).size };
			if (!codecs.includes('h264')) problems.push(`rtmp output has no h264 stream: ${codecs}`);
			if (duration < seconds - 3) problems.push(`rtmp output duration ${duration}s is short`);
		}
	}
} catch (error) {
	if (!(error instanceof Reported)) problems.push(String(error?.stack ?? error));
} finally {
	await browser.close();
}

console.log(JSON.stringify({ ok: problems.length === 0, ...result, problems }, null, 2));
process.exit(problems.length === 0 ? 0 : 1);

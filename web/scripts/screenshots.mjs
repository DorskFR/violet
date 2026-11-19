#!/usr/bin/env node
import { mkdir } from 'node:fs/promises';
import { parseArgs } from 'node:util';
import { chromium } from 'playwright';

const { values } = parseArgs({
	options: {
		url: { type: 'string', default: 'http://localhost:5173' },
		out: { type: 'string', default: '../.qa/web/shots' },
		readme: { type: 'string', default: '' },
		only: { type: 'string', default: '' },
	},
});

const VIEWPORTS = {
	desktop: { width: 1280, height: 900 },
	mobile: { width: 390, height: 844 },
};
const THEMES = ['light', 'dark'];

async function firstDocument(page, query) {
	return page.evaluate(async (q) => {
		const res = await fetch(`/api/search?limit=1&q=${encodeURIComponent(q)}`);
		return (await res.json()).hits[0].id;
	}, query);
}

const resetMock = (page, body) =>
	page.evaluate(
		(b) => fetch('/api/_mock/reset', { method: 'POST', body: JSON.stringify(b) }),
		body,
	);

async function signIn(page) {
	await resetMock(page, {});
	await page.evaluate(() =>
		fetch('/api/auth/login', {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ name: 'alex', password: 'violet-mock-password' }),
		}),
	);
}

async function addMailbox(page) {
	await page.getByRole('button', { name: 'Add connector' }).click();
	const form = page.getByRole('dialog');
	await form.getByLabel('Kind').selectOption('imap');
	await form.getByLabel('Name', { exact: true }).fill('home');
	await form.getByLabel('Username').fill('alex@example.com');
	await form.getByLabel('Host').fill('imap.example.com');
}

async function headerProblems(page) {
	return page.evaluate(() => {
		const header = document.querySelector('header')?.getBoundingClientRect();
		const nav = document.querySelector('header nav');
		if (!header || !nav) return [];
		const problems = [];
		for (const el of [nav, ...nav.querySelectorAll('a')]) {
			const r = el.getBoundingClientRect();
			if (r.top < header.top || r.bottom > header.bottom) {
				problems.push(
					`${el.textContent.trim() || 'nav'} spans ${r.top}-${r.bottom}, header ${header.top}-${header.bottom}`,
				);
			}
		}
		const row = nav.getBoundingClientRect();
		for (const link of nav.querySelectorAll('a')) {
			const r = link.getBoundingClientRect();
			if (r.left < row.left - 0.5 || r.right > row.right + 0.5) {
				problems.push(
					`${link.textContent.trim()} is cut off by the nav (${r.left}-${r.right}, nav ${row.left}-${row.right})`,
				);
			}
		}
		return problems;
	});
}

const SCREENS = [
	{ name: 'search', path: '/' },
	{ name: 'search-query', path: '/?q=領収&area=Health' },
	{
		name: 'search-filters',
		path: '/?q=tax',
		mobileOnly: true,
		prepare: (page) => page.getByRole('button', { name: /Filters/ }).click(),
	},
	{ name: 'document', document: 'Employment' },
	{
		name: 'document-edit',
		document: '診療明細書',
		prepare: (page) => page.getByRole('button', { name: 'Edit', exact: true }).click(),
	},
	{
		name: 'document-text',
		document: 'Amazon',
		prepare: (page) => page.getByRole('button', { name: 'Text', exact: true }).click(),
	},
	{
		name: 'document-split',
		document: 'Employment',
		prepare: async (page) => {
			await page.getByRole('button', { name: 'More actions' }).click();
			await page.getByRole('menuitem', { name: /Split/ }).click();
			await page.getByRole('button', { name: 'Cut after page 2' }).click();
			await page.getByRole('button', { name: 'Cut after page 4' }).click();
		},
	},
	{
		name: 'document-merge',
		document: '調剤',
		prepare: async (page) => {
			await page.getByRole('button', { name: 'More actions' }).click();
			await page.getByRole('menuitem', { name: /Merge/ }).click();
			await page.getByLabel('Find documents to merge').fill('医療');
			await page.locator('label[for^="merge-"]').last().click();
		},
	},
	{ name: 'activity', path: '/activity' },
	{ name: 'status', path: '/status' },
	{ name: 'login', path: '/login', before: (page) => resetMock(page, { mode: 'required' }) },
	{
		name: 'login-setup',
		path: '/login',
		before: (page) => resetMock(page, { mode: 'required', setup: true }),
	},
	{ name: 'account', path: '/account', before: signIn },
	{ name: 'settings', path: '/settings', before: signIn },
	{
		name: 'settings-connectors',
		path: '/settings',
		before: signIn,
		prepare: (page) =>
			page
				.getByRole('heading', { name: 'Connectors' })
				.evaluate((heading) => heading.scrollIntoView({ block: 'center' })),
	},
	{
		name: 'settings-connector-add',
		path: '/settings',
		before: signIn,
		prepare: (page) => page.getByRole('button', { name: 'Add connector' }).click(),
	},
	{
		name: 'settings-connector-add-imap',
		path: '/settings',
		before: signIn,
		prepare: (page) => addMailbox(page),
	},
	{
		name: 'settings-connector-add-imap-rules',
		path: '/settings',
		before: signIn,
		prepare: async (page) => {
			await addMailbox(page);
			await page.getByRole('button', { name: 'Add rule' }).click();
			await page
				.getByRole('group', { name: 'Rule 1' })
				.evaluate((rule) => rule.scrollIntoView({ block: 'start' }));
		},
	},
	{
		name: 'settings-connector-edit',
		path: '/settings',
		before: signIn,
		prepare: (page) => page.getByRole('button', { name: 'Edit mf' }).click(),
	},
	{
		name: 'settings-connector-code',
		path: '/settings',
		before: signIn,
		prepare: (page) => page.getByRole('button', { name: 'Connect', exact: true }).click(),
	},
];

await mkdir(values.out, { recursive: true });
if (values.readme) await mkdir(values.readme, { recursive: true });

const browser = await chromium.launch();
try {
	for (const [size, viewport] of Object.entries(VIEWPORTS)) {
		for (const mode of THEMES) {
			const context = await browser.newContext({
				viewport,
				deviceScaleFactor: 2,
				colorScheme: mode,
				locale: 'en-US',
				hasTouch: size === 'mobile',
				isMobile: size === 'mobile',
			});
			const page = await context.newPage();
			page.on('console', (msg) => {
				if (msg.type() === 'error' || msg.type() === 'warning') {
					console.log(`[${size}/${mode}] ${msg.type()}: ${msg.text()}`);
				}
			});
			page.on('pageerror', (e) => console.log(`[${size}/${mode}] pageerror: ${e.message}`));
			await page.goto(values.url);
			for (const screen of SCREENS) {
				if (screen.mobileOnly && size !== 'mobile') continue;
				if (values.only && !values.only.split(',').some((p) => screen.name.startsWith(p))) continue;
				const file = `${values.out}/${screen.name}-${size}-${mode}.png`;
				try {
					const path = screen.document
						? `/documents/${await firstDocument(page, screen.document)}`
						: screen.path;
					await screen.before?.(page);
					await page.goto(new URL(path, values.url).href);
					await page.waitForLoadState('networkidle');
					await screen.prepare?.(page);
					await page.waitForLoadState('networkidle');
					await page.waitForTimeout(400);
					await page.screenshot({ path: file });
					console.log(file);
					for (const problem of await headerProblems(page)) {
						console.log(`LAYOUT ${file}: ${problem}`);
						process.exitCode = 1;
					}
					if (values.readme && screen.name === 'search' && size === 'desktop') {
						await page.screenshot({ path: `${values.readme}/screenshot-${mode}.png` });
					}
				} catch (e) {
					console.log(`FAILED ${file}: ${e.message.split('\n')[0]}`);
					await page.screenshot({ path: file }).catch(() => {});
				}
			}
			await context.close();
		}
	}
} finally {
	await browser.close();
}

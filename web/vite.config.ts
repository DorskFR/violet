import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vitest/config';
import { mockBackend } from './mock/plugin.ts';

const backend = process.env.VIOLET_BACKEND ?? 'http://localhost:8080';
const mock = process.env.VIOLET_MOCK === '1';

export default defineConfig({
	plugins: [
		...(mock ? [mockBackend()] : []),
		sveltekit({
			preprocess: vitePreprocess(),
			adapter: adapter({ pages: 'build', assets: 'build', fallback: 'index.html', strict: false }),
		}),
	],
	server: {
		proxy: mock ? undefined : { '/api': { target: backend } },
	},
	test: {
		include: ['src/**/*.test.ts', 'mock/**/*.test.ts'],
		environment: 'node',
	},
});

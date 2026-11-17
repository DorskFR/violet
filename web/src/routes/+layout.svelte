<script lang="ts">
	import '../app.css';
	import { AppShell, Link, NavBar, ThemePicker, Toaster, theme } from '@dorsk/tsumikit';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';

	let { children } = $props();

	if (!localStorage.getItem('tsumikit-theme')) {
		theme.hydrate({ mode: 'auto', light: 'light', dark: 'dark' });
	}

	const path = $derived(page.url.pathname);
	const items = $derived([
		{
			href: resolve('/'),
			label: 'Search',
			icon: 'search' as const,
			current: path === '/' || path.startsWith('/documents'),
		},
		{
			href: resolve('/activity'),
			label: 'Activity',
			icon: 'clock' as const,
			current: path.startsWith('/activity'),
		},
		{
			href: resolve('/status'),
			label: 'Inbox',
			icon: 'upload' as const,
			current: path.startsWith('/status'),
		},
	]);
</script>

<svelte:head>
	<title>violet</title>
</svelte:head>

<AppShell sidebarWidth="0px">
	{#snippet header()}
		<div class="bar">
			<Link href={resolve('/')} tone="inherit" underline="none" class="brand">violet</Link>
			<NavBar {items} label="Main" orientation="inline" labels="full" />
			<div class="end">
				<ThemePicker auto variant="ghost" placement="bottom-end" />
			</div>
		</div>
	{/snippet}
	<div class="content">
		{@render children()}
	</div>
</AppShell>
<Toaster />

<style>
	.bar {
		display: flex;
		align-items: center;
		gap: var(--sp-3);
		inline-size: 100%;
		max-inline-size: 84rem;
		margin-inline: auto;
	}
	.bar :global(.brand) {
		font-weight: 650;
		font-size: var(--fs-md);
		letter-spacing: 0.01em;
	}
	.end {
		margin-inline-start: auto;
	}
	.content {
		box-sizing: border-box;
		padding-inline: var(--sp-4);
		inline-size: 100%;
		max-inline-size: 84rem;
		margin-inline: auto;
	}
</style>

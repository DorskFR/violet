<script lang="ts">
	import '../app.css';
	import {
		AppShell,
		Badge,
		Callout,
		Link,
		NavBar,
		Text,
		ThemePicker,
		Toaster,
		theme,
	} from '@dorsk/tsumikit';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { ApiError, api, type Me } from '#lib/api';
	import { isAdmin } from '#lib/users';

	let { children } = $props();

	if (!localStorage.getItem('tsumikit-theme')) {
		theme.hydrate({ mode: 'auto', light: 'light', dark: 'dark' });
	}

	let me = $state<Me | null>(null);
	let signedOut = $state('');

	api.me().then(
		(value) => {
			me = value;
		},
		(e) => {
			if (e instanceof ApiError && e.status === 401) signedOut = e.message;
		},
	);

	const path = $derived(page.url.pathname);
	const base = $derived([
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
	const items = $derived(
		isAdmin(me)
			? [
					...base,
					{
						href: resolve('/settings'),
						label: 'Settings',
						icon: 'settings' as const,
						current: path.startsWith('/settings'),
					},
				]
			: base,
	);
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
				{#if signedOut}
					<Badge size="sm" tone="warn" icon="lock">Not signed in</Badge>
				{:else if me?.name}
					<Text size="sm" tone="muted" title="Signed in as {me.name}">{me.name}</Text>
				{/if}
				<ThemePicker auto variant="ghost" placement="bottom-end" />
			</div>
		</div>
	{/snippet}
	<div class="content">
		{#if signedOut}
			<Callout tone="warn" title="Not signed in">{signedOut}</Callout>
		{/if}
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
		display: flex;
		align-items: center;
		gap: var(--sp-2);
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

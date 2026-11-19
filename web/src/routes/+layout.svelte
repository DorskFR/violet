<script lang="ts">
	import '../app.css';
	import {
		AppShell,
		Button,
		IconButton,
		Link,
		NavBar,
		Text,
		ThemePicker,
		Toaster,
		theme,
		toasts,
	} from '@dorsk/tsumikit';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { api, errorMessage, onUnauthorized } from '#lib/api';
	import { hasSession, loginHref } from '#lib/auth';
	import { session, signedInAs } from '#lib/session.svelte.ts';
	import { canWrite } from '#lib/users';

	let { children } = $props();

	if (!localStorage.getItem('tsumikit-theme')) {
		theme.hydrate({ mode: 'auto', light: 'light', dark: 'dark' });
	}

	onUnauthorized(() => {
		signedInAs(null);
		if (location.pathname !== '/login') goto(loginHref(location.pathname + location.search));
	});

	if (location.pathname !== '/login') api.me().then(signedInAs, () => {});

	let signingOut = $state(false);

	async function signOut() {
		signingOut = true;
		try {
			await api.logout();
			signedInAs(null);
			await goto(resolve('/login'));
		} catch (e) {
			toasts.error(errorMessage(e, 'Signing out failed.'));
		} finally {
			signingOut = false;
		}
	}

	const me = $derived(session.me);
	const path = $derived(page.url.pathname);
	const onLogin = $derived(path === '/login');
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
		canWrite(me)
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
			{#if !onLogin}
				<NavBar {items} label="Main" orientation="inline" labels="full" />
			{/if}
			<div class="end">
				{#if me?.name && hasSession(me)}
					<span class="wide">
						<Link href={resolve('/account')} tone="muted" underline="hover" title="Your account">
							{me.name}
						</Link>
						<Button size="sm" variant="ghost" loading={signingOut} onclick={signOut}>Sign out</Button>
					</span>
					<IconButton
						class="compact"
						icon="user"
						label="Account of {me.name}"
						variant="ghost"
						href={resolve('/account')}
					/>
				{:else if me?.name}
					<Text size="sm" tone="muted" title="Signed in as {me.name}">{me.name}</Text>
				{/if}
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
		--nav-h: var(--touch-target);
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
	.wide {
		display: flex;
		align-items: center;
		gap: var(--sp-2);
	}
	.end :global(.compact) {
		display: none;
	}
	@media (max-width: 48rem) {
		.wide {
			display: none;
		}
		.end :global(.compact) {
			display: inline-flex;
		}
		.bar :global(.navbar-link) {
			min-inline-size: var(--touch-target);
		}
		.bar :global(.navbar-label) {
			position: absolute;
			inset-block-start: 0;
			inset-inline-start: 0;
			inline-size: 1px;
			block-size: 1px;
			overflow: hidden;
			clip-path: inset(50%);
			white-space: nowrap;
		}
	}
	.content {
		box-sizing: border-box;
		padding-inline: var(--sp-4);
		inline-size: 100%;
		max-inline-size: 84rem;
		margin-inline: auto;
	}
</style>

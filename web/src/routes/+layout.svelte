<script lang="ts">
	import '../app.css';
	import {
		AppShell,
		Button,
		Callout,
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
	import { hasSession, loginHref, mustSetUp, needsPasskey, signInEntry } from '#lib/auth';
	import { addPasskey } from '#lib/passkeys';
	import { loadLogin, session, signedInAs } from '#lib/session.svelte.ts';
	import { passkeyError, passkeysSupported } from '#lib/webauthn';
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
	loadLogin();

	let signingOut = $state(false);
	let adding = $state(false);
	let later = $state(sessionStorage.getItem('violet-passkey-later') === '1');

	async function addHere() {
		adding = true;
		try {
			const passkey = await addPasskey();
			signedInAs(await api.me());
			toasts.ok(`Passkey added: next time, sign in with ${passkey.name}.`);
		} catch (e) {
			toasts.error(passkeyError(e, 'The passkey could not be added.'));
		} finally {
			adding = false;
		}
	}

	function notNow() {
		sessionStorage.setItem('violet-passkey-later', '1');
		later = true;
	}

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

	$effect(() => {
		if (mustSetUp(session.login, path)) goto(resolve('/login'));
	});

	const offerPasskey = $derived(
		needsPasskey(me) && passkeysSupported() && !later && !path.startsWith('/account'),
	);
	const entry = $derived(onLogin ? null : signInEntry(session.login, me));
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
						variant={needsPasskey(me) ? 'primary' : 'default'}
						href={resolve('/account')}
					/>
				{:else if entry}
					<span class="wide">
						<Button size="sm" href={loginHref(page.url.pathname + page.url.search)}>
							{entry}
						</Button>
					</span>
					<IconButton
						class="compact"
						icon="user"
						label={entry}
						variant="primary"
						href={loginHref(page.url.pathname + page.url.search)}
					/>
				{:else if me?.name}
					<Text size="sm" tone="muted" title="Signed in as {me.name}">{me.name}</Text>
				{/if}
				<ThemePicker auto variant="ghost" placement="bottom-end" />
			</div>
		</div>
	{/snippet}
	<div class="content">
		{#if offerPasskey}
			<div class="offer">
				<Callout tone="info" icon="key-round" title="Add a passkey">
					Sign in with your fingerprint or screen lock instead of a password.
					{#snippet actions()}
						<Button size="sm" variant="primary" loading={adding} onclick={addHere}>
							Add a passkey
						</Button>
						<Button size="sm" variant="ghost" disabled={adding} onclick={notNow}>Not now</Button>
					{/snippet}
				</Callout>
			</div>
		{/if}
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
	.offer {
		padding-block-start: var(--sp-3);
	}
	.content {
		box-sizing: border-box;
		padding-inline: var(--sp-4);
		inline-size: 100%;
		max-inline-size: 84rem;
		margin-inline: auto;
	}
</style>

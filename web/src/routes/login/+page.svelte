<script lang="ts">
	import {
		Button,
		Callout,
		Card,
		EmptyState,
		Field,
		Heading,
		Input,
		Skeleton,
		Stack,
		Text,
	} from '@dorsk/tsumikit';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { type AuthState, api, errorMessage, type Me } from '#lib/api';
	import { hasSession, safeNext, signInError } from '#lib/auth';
	import { signInWithPasskey, signUp } from '#lib/passkeys';
	import { loginIs, signedInAs } from '#lib/session.svelte.ts';
	import { passkeyError, passkeysSupported } from '#lib/webauthn';

	const uid = $props.id();

	let auth = $state<AuthState | null>(null);
	let loadError = $state('');
	let name = $state('');
	let password = $state('');
	let withPassword = $state(false);
	let busy = $state(false);
	let failure = $state('');

	const next = $derived(safeNext(page.url.searchParams.get('next')));
	const canPasskey = passkeysSupported();

	async function load() {
		loadError = '';
		try {
			auth = await api.auth();
			loginIs(auth);
			if (auth.user) signedInAs(auth.user);
		} catch (e) {
			loadError = errorMessage(e, 'The sign-in page could not be loaded.');
		}
	}

	load();

	async function signedIn(me: Me) {
		signedInAs(me);
		if (auth) loginIs({ mode: auth.mode, setup: false });
		await goto(next);
	}

	async function attempt(run: () => Promise<Me>, fallback: string) {
		busy = true;
		failure = '';
		try {
			await signedIn(await run());
		} catch (e) {
			failure = e instanceof DOMException ? passkeyError(e, fallback) : signInError(e, fallback);
			password = '';
		} finally {
			busy = false;
		}
	}

	function create(event: SubmitEvent) {
		event.preventDefault();
		const who = name.trim();
		if (!who) {
			failure = 'Choose a name to sign in with.';
			return;
		}
		attempt(() => signUp(who), 'The account could not be created.');
	}

	function signIn(event: SubmitEvent) {
		event.preventDefault();
		const who = name.trim();
		if (!withPassword) {
			attempt(() => signInWithPasskey(who || undefined), 'Signing in failed.');
			return;
		}
		if (!who || !password) {
			failure = 'Type your name and your password.';
			return;
		}
		attempt(() => api.login(who, password), 'Signing in failed.');
	}
</script>

<svelte:head>
	<title>{auth?.setup ? 'Create your account' : 'Sign in'} · violet</title>
</svelte:head>

<div class="page login">
	{#if loadError}
		<EmptyState
			tone="danger"
			icon="alert-circle"
			title="The sign-in page did not load"
			description={loadError}
			actionLabel="Try again"
			onAction={load}
		/>
	{:else if !auth}
		<Skeleton lines={6} />
	{:else if auth.mode === 'off'}
		<Callout tone="info" title="Signing in is turned off">
			This server does not use accounts. Scripts and devices use a token.
		</Callout>
		<div>
			<Button variant="primary" onclick={() => goto(next)}>Continue</Button>
		</div>
	{:else if hasSession(auth.user) && auth.user}
		<Callout tone="ok" title="You are signed in">Signed in as {auth.user.name}.</Callout>
		<div>
			<Button variant="primary" onclick={() => goto(next)}>Continue</Button>
		</div>
	{:else if auth.setup}
		<Card padding="lg">
			<form class="form" onsubmit={create} novalidate>
				<Stack gap="var(--sp-3)">
					<Heading level={1} size="lg">Create your account</Heading>
					<Text as="p" tone="muted" size="sm">
						You become the admin. This device keeps a passkey for you: no password.
					</Text>
					<Field label="Name" for="{uid}-name">
						<Input
							id="{uid}-name"
							bind:value={name}
							autocomplete="username"
							autocapitalize="none"
							spellcheck={false}
							required
						/>
					</Field>
					{#if failure}
						<Text as="p" tone="danger" size="sm" role="alert">{failure}</Text>
					{/if}
					{#if canPasskey}
						<Button type="submit" variant="primary" block loading={busy}>
							Create with a passkey
						</Button>
					{:else}
						<Callout tone="warn" title="This browser cannot create passkeys">
							Open violet in a recent browser on your phone or computer.
						</Callout>
					{/if}
				</Stack>
			</form>
		</Card>
	{:else}
		<Card padding="lg">
			<form class="form" onsubmit={signIn} novalidate>
				<Stack gap="var(--sp-3)">
					<Heading level={1} size="lg">Sign in</Heading>
					<Field
						label="Name"
						for="{uid}-name"
						hint={withPassword ? undefined : 'Optional with a passkey.'}
					>
						<Input
							id="{uid}-name"
							bind:value={name}
							autocomplete="username webauthn"
							autocapitalize="none"
							spellcheck={false}
						/>
					</Field>
					{#if withPassword}
						<Field label="Password" for="{uid}-password">
							<Input
								id="{uid}-password"
								type="password"
								bind:value={password}
								autocomplete="current-password"
							/>
						</Field>
					{/if}
					{#if failure}
						<Text as="p" tone="danger" size="sm" role="alert">{failure}</Text>
					{/if}
					{#if withPassword || !canPasskey}
						<Button type="submit" variant="primary" block loading={busy}>
							Sign in with the password
						</Button>
					{:else}
						<Button type="submit" variant="primary" block loading={busy}>
							Sign in with a passkey
						</Button>
					{/if}
					{#if canPasskey}
						<Button
							variant="ghost"
							size="sm"
							disabled={busy}
							onclick={() => {
								withPassword = !withPassword;
								failure = '';
							}}
						>
							{withPassword ? 'Use a passkey instead' : 'Use a password instead'}
						</Button>
					{/if}
				</Stack>
			</form>
		</Card>
	{/if}
</div>

<style>
	.login {
		max-inline-size: 26rem;
		margin-inline: auto;
		padding-block-start: var(--sp-10);
	}
	.form {
		display: contents;
	}
</style>

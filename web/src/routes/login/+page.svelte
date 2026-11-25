<script lang="ts">
	import {
		Button,
		Callout,
		Card,
		Divider,
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
	import { afterSignIn, hasSession, passwordProblem, safeNext, signInError } from '#lib/auth';
	import { loginIs, signedInAs } from '#lib/session.svelte.ts';
	import { passkeyError, passkeysSupported, usePasskey } from '#lib/webauthn';

	const uid = $props.id();

	let auth = $state<AuthState | null>(null);
	let loadError = $state('');
	let name = $state('');
	let password = $state('');
	let confirm = $state('');
	let busy = $state<'password' | 'passkey' | null>(null);
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
		const setup = auth?.setup ?? false;
		signedInAs(me);
		if (auth) loginIs({ mode: auth.mode, setup: false });
		await goto(afterSignIn(setup, next));
	}

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		if (!auth) return;
		const who = name.trim();
		if (!who) {
			failure = 'Type your name.';
			return;
		}
		if (auth.setup) {
			const problem = passwordProblem(password, who, confirm);
			if (problem) {
				failure = problem;
				return;
			}
		} else if (!password) {
			failure = 'Type your password, or use a passkey.';
			return;
		}
		busy = 'password';
		failure = '';
		try {
			await signedIn(auth.setup ? await api.setup(who, password) : await api.login(who, password));
		} catch (e) {
			failure = signInError(e, 'Signing in failed.');
			password = '';
		} finally {
			busy = null;
		}
	}

	async function withPasskey() {
		const who = name.trim();
		if (!who) {
			failure = 'Type your name first, then use your passkey.';
			return;
		}
		busy = 'passkey';
		failure = '';
		try {
			const { ceremony, options } = await api.passkeyLoginStart(who);
			const credential = await usePasskey(options.publicKey);
			await signedIn(await api.passkeyLoginFinish(ceremony, credential));
		} catch (e) {
			failure = e instanceof DOMException ? passkeyError(e, '') : signInError(e, 'Signing in failed.');
		} finally {
			busy = null;
		}
	}
</script>

<svelte:head>
	<title>{auth?.setup ? 'Create the admin account' : 'Sign in'} · violet</title>
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
	{:else}
		<Card padding="lg">
			<form class="form" onsubmit={submit} novalidate>
				<Stack gap="var(--sp-3)">
					<Heading level={1} size="lg">
						{auth.setup ? 'Create the admin account' : 'Sign in'}
					</Heading>
					{#if auth.setup}
						<Text as="p" tone="muted" size="sm">
							Nobody can sign in yet. This account becomes the admin; use the name of an existing
							user to claim it. Your account page opens next, to add a passkey.
						</Text>
					{/if}
					<Field label="Name" for="{uid}-name">
						<Input
							id="{uid}-name"
							bind:value={name}
							autocomplete={auth.setup ? 'username' : 'username webauthn'}
							autocapitalize="none"
							spellcheck={false}
							required
						/>
					</Field>
					<Field
						label="Password"
						for="{uid}-password"
						hint={auth.setup ? 'At least 12 characters.' : undefined}
					>
						<Input
							id="{uid}-password"
							type="password"
							bind:value={password}
							autocomplete={auth.setup ? 'new-password' : 'current-password'}
							required
						/>
					</Field>
					{#if auth.setup}
						<Field label="Password again" for="{uid}-confirm">
							<Input
								id="{uid}-confirm"
								type="password"
								bind:value={confirm}
								autocomplete="new-password"
								required
							/>
						</Field>
					{/if}
					{#if failure}
						<Text as="p" tone="danger" size="sm" role="alert">{failure}</Text>
					{/if}
					<Button
						type="submit"
						variant="primary"
						block
						loading={busy === 'password'}
						disabled={busy !== null}
					>
						{auth.setup ? 'Create the admin account' : 'Sign in'}
					</Button>
					{#if !auth.setup && canPasskey}
						<Divider decorative>or</Divider>
						<Button
							block
							loading={busy === 'passkey'}
							disabled={busy !== null}
							onclick={withPasskey}
						>
							Use a passkey
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

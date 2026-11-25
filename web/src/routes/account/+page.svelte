<script lang="ts">
	import {
		Badge,
		Button,
		Callout,
		ConfirmModal,
		EmptyState,
		Field,
		Heading,
		IconButton,
		Input,
		Modal,
		SectionHeader,
		Skeleton,
		Stack,
		Text,
		Timestamp,
		toasts,
	} from '@dorsk/tsumikit';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { api, errorMessage, type Passkey, type Session } from '#lib/api';
	import { hasSession, loginHref, passwordProblem } from '#lib/auth';
	import { session, signedInAs } from '#lib/session.svelte.ts';
	import { createPasskey, passkeyError, passkeysSupported } from '#lib/webauthn';

	const uid = $props.id();
	const canPasskey = passkeysSupported();

	const me = $derived(session.me);

	let passkeys = $state<Passkey[] | null>(null);
	let sessions = $state<Session[] | null>(null);
	let error = $state('');

	let current = $state('');
	let password = $state('');
	let confirm = $state('');
	let passwordBusy = $state(false);
	let passwordFailure = $state('');

	let keyName = $state('');
	let keyBusy = $state(false);
	let keyFailure = $state('');

	let renaming = $state<Passkey | null>(null);
	let renameOpen = $state(false);
	let renameTo = $state('');
	let renameBusy = $state(false);
	let renameFailure = $state('');

	let removing = $state<Passkey | null>(null);
	let removeOpen = $state(false);

	let ending = $state<Session | null>(null);
	let endOpen = $state(false);

	async function load() {
		error = '';
		try {
			const who = await api.me();
			signedInAs(who);
			if (!hasSession(who)) return;
			const [k, s] = await Promise.all([api.passkeys(), api.sessions()]);
			passkeys = k;
			sessions = s;
		} catch (e) {
			error = errorMessage(e, 'Your account could not be loaded.');
		}
	}

	async function refreshMe() {
		signedInAs(await api.me());
	}

	load();

	async function changePassword(event: SubmitEvent) {
		event.preventDefault();
		if (!me?.name) return;
		if (me.password && !current) {
			passwordFailure = 'Type your current password.';
			return;
		}
		const problem = passwordProblem(password, me.name, confirm);
		if (problem) {
			passwordFailure = problem;
			return;
		}
		passwordBusy = true;
		passwordFailure = '';
		try {
			await api.changePassword(password, me.password ? current : undefined);
			current = password = confirm = '';
			await refreshMe();
			await load();
			toasts.ok('Password changed. Your other sessions that used the old one have ended.');
		} catch (e) {
			passwordFailure = errorMessage(e, 'The password could not be changed.');
		} finally {
			passwordBusy = false;
		}
	}

	async function addPasskey(event: SubmitEvent) {
		event.preventDefault();
		const name = keyName.trim();
		if (!name) {
			keyFailure = 'Name the passkey after the device that keeps it, such as phone.';
			return;
		}
		keyBusy = true;
		keyFailure = '';
		try {
			const { ceremony, options } = await api.passkeyAddStart(name);
			const credential = await createPasskey(options.publicKey);
			await api.passkeyAddFinish(ceremony, credential);
			keyName = '';
			await refreshMe();
			await load();
			toasts.ok(`Passkey ${name} added`);
		} catch (e) {
			keyFailure = passkeyError(e, 'The passkey could not be added.');
		} finally {
			keyBusy = false;
		}
	}

	function openRename(key: Passkey) {
		renaming = key;
		renameTo = key.name;
		renameFailure = '';
		renameOpen = true;
	}

	async function rename() {
		if (!renaming) return;
		const to = renameTo.trim();
		if (!to) {
			renameFailure = 'A passkey needs a name.';
			return;
		}
		renameBusy = true;
		renameFailure = '';
		try {
			if (to !== renaming.name) await api.renamePasskey(renaming.name, to);
			renameOpen = false;
			await refreshMe();
			await load();
		} catch (e) {
			renameFailure = errorMessage(e, 'The passkey could not be renamed.');
		} finally {
			renameBusy = false;
		}
	}

	async function removePasskey() {
		if (!removing) return;
		const name = removing.name;
		try {
			await api.removePasskey(name);
			await refreshMe();
			await load();
			toasts.ok(`Passkey ${name} removed`);
		} catch (e) {
			toasts.error(errorMessage(e, 'The passkey could not be removed.'));
		}
	}

	async function endSession() {
		if (!ending) return;
		const target = ending;
		try {
			await api.endSession(target.id);
			if (target.current) {
				signedInAs(null);
				await goto(resolve('/login'));
				return;
			}
			await load();
		} catch (e) {
			toasts.error(errorMessage(e, 'The session could not be ended.'));
		}
	}
</script>

<svelte:head>
	<title>Account · violet</title>
</svelte:head>

<div class="page narrow">
	<Heading level={1} size="lg">{me?.name ? `Account of ${me.name}` : 'Account'}</Heading>

	{#if error && !passkeys}
		<EmptyState
			tone="danger"
			icon="alert-circle"
			title="Your account did not load"
			description={error}
			actionLabel="Try again"
			onAction={load}
		/>
	{:else if !me}
		<Skeleton lines={6} />
	{:else if !hasSession(me)}
		{#if session.login?.setup}
			<Callout tone="info" title="Create your account first">
				Nobody can sign in yet. Create the admin account, then add a passkey here.
				{#snippet actions()}
					<Button size="sm" variant="primary" href={loginHref('/account')}>Create account</Button>
				{/snippet}
			</Callout>
		{:else}
			<Callout tone="info" title="Sign in first">
				Passwords, passkeys and sessions belong to an account. Sign in with yours to manage them.
				{#snippet actions()}
					<Button size="sm" variant="primary" href={loginHref('/account')}>Sign in</Button>
				{/snippet}
			</Callout>
		{/if}
	{:else if !passkeys || !sessions}
		<Skeleton lines={8} />
	{:else}
		<Stack gap="var(--sp-2)">
			<SectionHeader title="Password" level={2} size="sm" />
			<form class="form" onsubmit={changePassword} novalidate>
				{#if me.password}
					<Field label="Current password" for="{uid}-current">
						<Input
							id="{uid}-current"
							type="password"
							bind:value={current}
							autocomplete="current-password"
						/>
					</Field>
				{:else}
					<Text as="p" tone="muted" size="sm">You sign in with a passkey only. A password is optional.</Text>
				{/if}
				<Field label="New password" for="{uid}-new" hint="At least 12 characters.">
					<Input id="{uid}-new" type="password" bind:value={password} autocomplete="new-password" />
				</Field>
				<Field label="New password again" for="{uid}-confirm">
					<Input
						id="{uid}-confirm"
						type="password"
						bind:value={confirm}
						autocomplete="new-password"
					/>
				</Field>
				{#if passwordFailure}
					<Text as="p" tone="danger" size="sm" role="alert">{passwordFailure}</Text>
				{/if}
				<div>
					<Button type="submit" variant="primary" size="sm" loading={passwordBusy}>
						{me.password ? 'Change password' : 'Set password'}
					</Button>
				</div>
			</form>
		</Stack>

		<Stack gap="var(--sp-2)">
			<SectionHeader title="Passkeys" level={2} size="sm" count={passkeys.length || undefined} />
			{#if passkeys.length === 0}
				<Text as="p" tone="muted" size="sm">No passkey yet.</Text>
			{:else}
				<ul class="plain-list rows">
					{#each passkeys as key (key.name)}
						<li class="row">
							<div class="grow">
								<Text weight="medium" wrap="anywhere">{key.name}</Text>
								<Text size="sm" tone="muted">
									added <Timestamp value={key.created} mode="relative" size="sm" tone="muted" />
								</Text>
							</div>
							<Button size="sm" onclick={() => openRename(key)}>Rename</Button>
							<IconButton
								icon="trash"
								label="Remove passkey {key.name}"
								variant="ghost"
								box="sm"
								hoverDanger
								onclick={() => {
									removing = key;
									removeOpen = true;
								}}
							/>
						</li>
					{/each}
				</ul>
			{/if}
			{#if canPasskey}
				<form class="add" onsubmit={addPasskey} novalidate>
					<Field label="Name of a new passkey" for="{uid}-key" grow>
						<Input id="{uid}-key" bind:value={keyName} placeholder="phone" autocomplete="off" />
					</Field>
					<Button type="submit" size="sm" loading={keyBusy}>Add a passkey</Button>
				</form>
				{#if keyFailure}
					<Text as="p" tone="danger" size="sm" role="alert">{keyFailure}</Text>
				{/if}
			{:else}
				<Text as="p" tone="muted" size="sm">This browser cannot create passkeys.</Text>
			{/if}
		</Stack>

		<Stack gap="var(--sp-2)">
			<SectionHeader title="Sessions" level={2} size="sm" count={sessions.length || undefined} />
			<Text as="p" tone="muted" size="sm">
				Each browser you signed in from. A session ends after 30 days without use.
			</Text>
			<ul class="plain-list rows">
				{#each sessions as s (s.id)}
					<li class="row">
						<div class="grow">
							<div class="line">
								<Text weight="medium" wrap="anywhere">{s.label || 'Unknown browser'}</Text>
								{#if s.current}<Badge size="xs" tone="violet">this browser</Badge>{/if}
								<Badge size="xs" tone="muted">{s.credential}</Badge>
							</div>
							<div class="line">
								<Text size="sm" tone="muted">
									signed in <Timestamp value={s.created} mode="relative" size="sm" tone="muted" />
								</Text>
								<Text size="sm" tone="muted">
									last used <Timestamp value={s.last_seen} mode="relative" size="sm" tone="muted" />
								</Text>
							</div>
						</div>
						<Button
							size="sm"
							variant={s.current ? 'default' : 'danger'}
							onclick={() => {
								ending = s;
								endOpen = true;
							}}
						>
							{s.current ? 'Sign out' : 'End'}
						</Button>
					</li>
				{/each}
			</ul>
		</Stack>
	{/if}
</div>

<Modal bind:open={renameOpen} title="Rename passkey" busy={renameBusy}>
	{#snippet body()}
		<Stack gap="var(--sp-3)">
			<Field label="Name" for="{uid}-rename">
				<Input id="{uid}-rename" bind:value={renameTo} autocomplete="off" onenter={rename} />
			</Field>
			{#if renameFailure}
				<Text as="p" tone="danger" size="sm" role="alert">{renameFailure}</Text>
			{/if}
		</Stack>
	{/snippet}
	{#snippet footer()}
		<Button onclick={() => (renameOpen = false)} disabled={renameBusy}>Cancel</Button>
		<Button variant="primary" loading={renameBusy} onclick={rename}>Rename</Button>
	{/snippet}
</Modal>

<ConfirmModal
	bind:open={removeOpen}
	title="Remove this passkey?"
	message={removing
		? `You can no longer sign in with ${removing.name}, and its sessions end.`
		: ''}
	confirmLabel="Remove"
	tone="danger"
	onconfirm={removePasskey}
/>

<ConfirmModal
	bind:open={endOpen}
	title={ending?.current ? 'Sign out of this browser?' : 'End this session?'}
	message={ending ? `${ending.label || 'That browser'} will have to sign in again.` : ''}
	confirmLabel={ending?.current ? 'Sign out' : 'End session'}
	tone="danger"
	onconfirm={endSession}
/>

<style>
	.narrow {
		max-inline-size: 48rem;
	}
	.form {
		display: flex;
		flex-direction: column;
		gap: var(--sp-3);
		max-inline-size: 24rem;
	}
	.add {
		display: flex;
		align-items: flex-end;
		gap: var(--sp-2);
		max-inline-size: 32rem;
	}
	.rows > li + li {
		border-block-start: 1px solid var(--border);
	}
	.row {
		display: flex;
		align-items: center;
		gap: var(--sp-3);
		padding-block: var(--sp-3);
	}
	.line {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--sp-2);
	}
	.grow {
		display: flex;
		flex-direction: column;
		flex: 1;
		min-inline-size: 0;
	}
</style>

<script lang="ts">
	import {
		Badge,
		Button,
		Callout,
		Checkbox,
		ConfirmModal,
		CopyButton,
		EmptyState,
		Field,
		Heading,
		IconButton,
		Input,
		Link,
		Modal,
		SectionHeader,
		Select,
		Skeleton,
		Stack,
		Text,
		Timestamp,
		toasts,
	} from '@dorsk/tsumikit';
	import { resolve } from '$app/paths';
	import {
		type Areas,
		api,
		errorMessage,
		type PrivateArea,
		type Role,
		type Scope,
		type User,
		type Users,
	} from '#lib/api';
	import { passwordProblem } from '#lib/auth';
	import Connectors from '#lib/components/Connectors.svelte';
	import { session } from '#lib/session.svelte.ts';
	import { announce } from '#lib/undo';
	import { cleanAreas, expiryFromDate, isAdmin, isExpired, parseNames } from '#lib/users';

	const uid = $props.id();

	const ROLES = [
		{ value: 'member', label: 'Member' },
		{ value: 'admin', label: 'Admin' },
	];
	const SCOPES = [
		{ value: 'read', label: 'Read: search and download' },
		{ value: 'write', label: 'Write: also upload and edit' },
		{ value: 'admin', label: 'Admin: also users and settings' },
	];

	let users = $state<Users | null>(null);
	let areas = $state<Areas | null>(null);
	let draft = $state<PrivateArea[]>([]);
	let error = $state('');
	let knownAreas = $state<string[]>([]);

	let userOpen = $state(false);
	let editing = $state<User | null>(null);
	let userName = $state('');
	let userRole = $state<Role>('member');
	let userPassword = $state('');
	let userConfirm = $state('');
	let userBusy = $state(false);
	let userFailure = $state('');

	let resetting = $state<User | null>(null);
	let resetOpen = $state(false);
	let resetPassword = $state('');
	let resetConfirm = $state('');
	let resetBusy = $state(false);
	let resetFailure = $state('');

	let removing = $state<User | null>(null);
	let removeOpen = $state(false);

	let tokenOpen = $state(false);
	let tokenUser = $state<User | null>(null);
	let tokenName = $state('');
	let tokenScope = $state<Scope>('write');
	let tokenExpiry = $state('');
	let tokenBusy = $state(false);
	let tokenFailure = $state('');
	let secret = $state('');

	let revoking = $state<{ user: string; token: string } | null>(null);
	let revokeOpen = $state(false);

	let savingAreas = $state(false);

	const names = $derived(users?.users.map((u) => u.name) ?? []);
	const areasDirty = $derived(
		areas !== null && JSON.stringify(cleanAreas(draft)) !== JSON.stringify(areas.private),
	);

	const undoable = {
		undo: (entry: string) => api.undo(entry),
		show: (message: string, options: Parameters<typeof toasts.show>[1]) =>
			toasts.show(message, options),
	};

	async function load() {
		error = '';
		try {
			const [u, a] = await Promise.all([api.users(), api.areas()]);
			users = u;
			areas = a;
			draft = a.private.map((p) => ({ area: p.area, members: [...p.members] }));
		} catch (e) {
			error = errorMessage(e, 'The settings could not be loaded.');
		}
	}

	async function loadKnownAreas() {
		try {
			knownAreas = (await api.facets()).areas.map((f) => f.value);
		} catch {
			knownAreas = [];
		}
	}

	const admin = $derived(isAdmin(session.me));

	$effect(() => {
		if (admin) {
			load();
			loadKnownAreas();
		}
	});

	function done(message: string, entry: string) {
		announce(undoable, `${message}. Activity can undo it.`, entry, load);
	}

	function openUser(user: User | null) {
		editing = user;
		userName = user?.name ?? '';
		userRole = user?.role ?? 'member';
		userPassword = '';
		userConfirm = '';
		userFailure = '';
		userOpen = true;
	}

	async function saveUser() {
		const name = userName.trim();
		if (!editing && !name) {
			userFailure = 'A user needs a name.';
			return;
		}
		if (!editing) {
			const problem = passwordProblem(userPassword, name, userConfirm);
			if (problem) {
				userFailure = problem;
				return;
			}
		}
		userBusy = true;
		userFailure = '';
		try {
			const { entry } = editing
				? await api.updateUser(editing.name, { role: userRole })
				: await api.addUser({
						name,
						role: userRole,
						password: userPassword,
					});
			userOpen = false;
			done(editing ? `${editing.name} updated` : `${name} added`, entry);
			await load();
		} catch (e) {
			userFailure = errorMessage(e, 'The user could not be saved.');
		} finally {
			userBusy = false;
		}
	}

	function openReset(user: User) {
		resetting = user;
		resetPassword = '';
		resetConfirm = '';
		resetFailure = '';
		resetOpen = true;
	}

	async function saveReset() {
		if (!resetting) return;
		const problem = passwordProblem(resetPassword, resetting.name, resetConfirm);
		if (problem) {
			resetFailure = problem;
			return;
		}
		resetBusy = true;
		resetFailure = '';
		try {
			const { entry } = await api.resetPassword(resetting.name, resetPassword);
			resetOpen = false;
			done(`Password of ${resetting.name} set`, entry);
			await load();
		} catch (e) {
			resetFailure = errorMessage(e, 'The password could not be set.');
		} finally {
			resetBusy = false;
		}
	}

	async function removeUser() {
		if (!removing) return;
		const name = removing.name;
		try {
			const { entry } = await api.removeUser(name);
			done(`${name} removed`, entry);
			await load();
		} catch (e) {
			toasts.error(errorMessage(e, 'The user could not be removed.'));
		}
	}

	function openToken(user: User) {
		tokenUser = user;
		tokenName = '';
		tokenScope = 'write';
		tokenExpiry = '';
		tokenFailure = '';
		secret = '';
		tokenOpen = true;
	}

	function closeToken() {
		tokenOpen = false;
		secret = '';
	}

	async function addToken() {
		if (!tokenUser) return;
		const name = tokenName.trim();
		if (!name) {
			tokenFailure = 'A token needs a name, such as the device or script that uses it.';
			return;
		}
		const expires = expiryFromDate(tokenExpiry);
		if (tokenExpiry && !expires) {
			tokenFailure = 'The expiry date is not a valid date.';
			return;
		}
		tokenBusy = true;
		tokenFailure = '';
		try {
			const created = await api.addToken(tokenUser.name, { name, scope: tokenScope, expires });
			secret = created.token;
			done(`Token ${name} added for ${tokenUser.name}`, created.entry);
			await load();
		} catch (e) {
			tokenFailure = errorMessage(e, 'The token could not be created.');
		} finally {
			tokenBusy = false;
		}
	}

	async function revokeToken() {
		if (!revoking) return;
		const { user, token } = revoking;
		try {
			const { entry } = await api.revokeToken(user, token);
			done(`Token ${token} revoked`, entry);
			await load();
		} catch (e) {
			toasts.error(errorMessage(e, 'The token could not be revoked.'));
		}
	}

	function passkeyCount(n: number): string {
		return n === 0 ? 'no passkey' : n === 1 ? '1 passkey' : `${n} passkeys`;
	}

	function memberChoices(area: PrivateArea): string[] {
		return [...names, ...area.members.filter((m) => !names.includes(m))];
	}

	function toggleMember(area: PrivateArea, name: string, on: boolean) {
		area.members = on
			? [...area.members.filter((m) => m !== name), name]
			: area.members.filter((m) => m !== name);
	}

	async function saveAreas() {
		savingAreas = true;
		try {
			const { entry } = await api.saveAreas({ private: cleanAreas(draft) });
			done('Private areas saved', entry);
			await load();
		} catch (e) {
			toasts.error(errorMessage(e, 'The private areas could not be saved.'));
		} finally {
			savingAreas = false;
		}
	}
</script>

<svelte:head>
	<title>Settings · violet</title>
</svelte:head>

<div class="page narrow">
	<Heading level={1} size="lg">Settings</Heading>

	{#if !session.me}
		<Skeleton lines={8} />
	{:else if !admin}
		<Connectors />
	{:else if error && !users}
		<EmptyState
			tone="danger"
			icon="alert-circle"
			title="The settings did not load"
			description={error}
			actionLabel="Try again"
			onAction={load}
		/>
	{:else if !users || !areas}
		<Skeleton lines={8} />
	{:else}
		<Stack gap="var(--sp-2)">
			<SectionHeader title="Users" level={2} size="sm" count={users.users.length || undefined}>
				{#snippet actions()}
					<Button size="sm" onclick={() => openUser(null)}>Add user</Button>
				{/snippet}
			</SectionHeader>
			<Text as="p" tone="muted" size="sm">
				Everyone adds their own passkeys from their account page, on the device that keeps them.
			</Text>
			{#if users.users.length === 0}
				<Text as="p" tone="muted" size="sm">No user is defined.</Text>
			{:else}
				<ul class="plain-list rows">
					{#each users.users as user (user.name)}
						<li class="user">
							<div class="head">
								<div class="grow">
									<div class="line">
										<Text weight="medium" wrap="anywhere">{user.name}</Text>
										<Badge size="xs" tone={user.role === 'admin' ? 'violet' : 'muted'}>{user.role}</Badge>
									</div>
									<div class="line">
										{#if user.password}
											<Badge size="xs" tone="muted" icon="lock">password</Badge>
										{:else if user.passkeys.length === 0}
											<Badge size="xs" tone="warn">cannot sign in</Badge>
										{/if}
										<Text size="sm" tone="muted">{passkeyCount(user.passkeys.length)}</Text>
									</div>
								</div>
								<Button size="sm" onclick={() => openUser(user)}>Edit</Button>
								{#if user.name === session.me?.name}
									<Button size="sm" href={resolve('/account')}>Your passkeys</Button>
								{:else}
									<Button size="sm" onclick={() => openReset(user)}>
										{user.password ? 'Reset password' : 'Set password'}
									</Button>
								{/if}
								<Button
									size="sm"
									variant="danger"
									onclick={() => {
										removing = user;
										removeOpen = true;
									}}
								>
									Remove
								</Button>
							</div>
							<div class="tokens">
								{#if user.tokens.length === 0}
									<Text size="sm" tone="faint">No token.</Text>
								{:else}
									<ul class="plain-list">
										{#each user.tokens as token (token.name)}
											<li class="token">
												<Text size="sm" wrap="anywhere">{token.name}</Text>
												<Badge size="xs" tone="muted">{token.scope}</Badge>
												{#if token.expires === null}
													<Text size="sm" tone="muted">never expires</Text>
												{:else if isExpired(token)}
													<Badge size="xs" tone="warn">expired</Badge>
												{:else}
													<Text size="sm" tone="muted">
														expires <Timestamp value={token.expires} mode="relative" size="sm" tone="muted" />
													</Text>
												{/if}
												<IconButton
													icon="trash"
													label="Revoke token {token.name} of {user.name}"
													variant="ghost"
													box="sm"
													hoverDanger
													onclick={() => {
														revoking = { user: user.name, token: token.name };
														revokeOpen = true;
													}}
												/>
											</li>
										{/each}
									</ul>
								{/if}
								<div>
									<Button size="sm" variant="ghost" onclick={() => openToken(user)}>
										Add token
									</Button>
								</div>
							</div>
						</li>
					{/each}
				</ul>
			{/if}
		</Stack>

		<Stack gap="var(--sp-2)">
			<SectionHeader title="Private areas" level={2} size="sm" count={draft.length || undefined} />
			<Text as="p" tone="muted" size="sm">
				Documents in a private area are visible to its members only. Areas not listed are visible to
				everyone.
			</Text>
			{#if draft.length > 0}
				<ul class="plain-list rows">
					{#each draft as area, i (i)}
						<li class="area">
							<div class="line">
								<Input
									bind:value={area.area}
									placeholder="Health/Sam"
									list="{uid}-areas"
									aria-label="Private area {i + 1}"
									autocomplete="off"
								/>
								<IconButton
									icon="trash"
									label="Remove private area {area.area || i + 1}"
									variant="ghost"
									box="sm"
									hoverDanger
									onclick={() => (draft = draft.filter((_, j) => j !== i))}
								/>
							</div>
							{#if users.configured && memberChoices(area).length > 0}
								<div class="members">
									{#each memberChoices(area) as name (name)}
										<Checkbox
											label={name}
											checked={area.members.includes(name)}
											onchange={(e) => toggleMember(area, name, e.currentTarget.checked)}
										/>
									{/each}
								</div>
							{:else}
								<Input
									value={area.members.join(', ')}
									placeholder="Members, separated by commas"
									aria-label="Members of private area {area.area || i + 1}"
									autocomplete="off"
									onchange={(e) => (area.members = parseNames(e.currentTarget.value))}
								/>
							{/if}
							{#if area.members.length === 0}
								<Text size="sm" tone="warn">Nobody is a member: nobody sees this area.</Text>
							{/if}
						</li>
					{/each}
				</ul>
			{:else}
				<Text as="p" tone="muted" size="sm">No area is private.</Text>
			{/if}
			<datalist id="{uid}-areas">
				{#each knownAreas as a (a)}<option value={a}></option>{/each}
			</datalist>
			<div class="actions">
				<Button
					size="sm"
					onclick={() => (draft = [...draft, { area: '', members: [] }])}
				>
					Add private area
				</Button>
				<Button
					size="sm"
					variant="primary"
					disabled={!areasDirty}
					loading={savingAreas}
					onclick={saveAreas}
				>
					Save private areas
				</Button>
			</div>
		</Stack>

		<Connectors />

		<Text as="p" tone="faint" size="xs">
			Every change here is journaled and can be undone from <Link href={resolve('/activity')}>Activity</Link>.
		</Text>
	{/if}
</div>

<Modal bind:open={userOpen} title={editing ? `Edit ${editing.name}` : 'Add user'} busy={userBusy}>
	{#snippet body()}
		<Stack gap="var(--sp-3)">
			{#if !editing}
				<Field label="Name" for="{uid}-name">
					<Input id="{uid}-name" bind:value={userName} autocomplete="off" />
				</Field>
			{/if}
			<Field label="Role" for="{uid}-role" hint="Admins manage users, tokens and private areas.">
				<Select id="{uid}-role" bind:value={userRole} options={ROLES} />
			</Field>
			{#if !editing}
				<Field
					label="First password"
					for="{uid}-password"
					hint="At least 12 characters. They sign in with it once, then add a passkey."
				>
					<Input
						id="{uid}-password"
						type="password"
						bind:value={userPassword}
						autocomplete="new-password"
					/>
				</Field>
				<Field label="Password again" for="{uid}-password-again">
					<Input
						id="{uid}-password-again"
						type="password"
						bind:value={userConfirm}
						autocomplete="new-password"
					/>
				</Field>
			{/if}
			{#if userFailure}
				<Text as="p" tone="danger" size="sm" role="alert">{userFailure}</Text>
			{/if}
		</Stack>
	{/snippet}
	{#snippet footer()}
		<Button onclick={() => (userOpen = false)} disabled={userBusy}>Cancel</Button>
		<Button variant="primary" loading={userBusy} onclick={saveUser}>
			{editing ? 'Save' : 'Add user'}
		</Button>
	{/snippet}
</Modal>

<Modal
	bind:open={tokenOpen}
	onclose={closeToken}
	title={tokenUser ? `New token for ${tokenUser.name}` : 'New token'}
	busy={tokenBusy}
>
	{#snippet body()}
		{#if secret}
			<Stack gap="var(--sp-3)">
				<Callout tone="warn" title="Copy the token now">
					You will not see it again. violet only keeps a hash of it.
				</Callout>
				<div class="secret">
					<Text mono wrap="anywhere" grow>{secret}</Text>
					<CopyButton text={secret} label="Copy token" />
				</div>
				<Text as="p" tone="muted" size="sm">Send it as the header Authorization: Bearer followed by the token.</Text>
			</Stack>
		{:else}
			<Stack gap="var(--sp-3)">
				<Field label="Name" for="{uid}-token" hint="The device or script that uses it.">
					<Input id="{uid}-token" bind:value={tokenName} placeholder="scanner" autocomplete="off" />
				</Field>
				<Field label="Scope" for="{uid}-scope">
					<Select id="{uid}-scope" bind:value={tokenScope} options={SCOPES} />
				</Field>
				<Field label="Expires" for="{uid}-expires" hint="At the end of that day. Empty: never.">
					<Input id="{uid}-expires" type="date" bind:value={tokenExpiry} />
				</Field>
				{#if tokenFailure}
					<Text as="p" tone="danger" size="sm" role="alert">{tokenFailure}</Text>
				{/if}
			</Stack>
		{/if}
	{/snippet}
	{#snippet footer()}
		{#if secret}
			<Button variant="primary" onclick={closeToken}>Done</Button>
		{:else}
			<Button onclick={closeToken} disabled={tokenBusy}>Cancel</Button>
			<Button variant="primary" loading={tokenBusy} onclick={addToken}>Create token</Button>
		{/if}
	{/snippet}
</Modal>

<Modal
	bind:open={resetOpen}
	title={resetting ? `Password of ${resetting.name}` : 'Password'}
	busy={resetBusy}
>
	{#snippet body()}
		<Stack gap="var(--sp-3)">
			<Text as="p" tone="muted" size="sm">
				Their sessions opened with the old password end. Tell them the new one in person.
			</Text>
			<Field label="New password" for="{uid}-reset" hint="At least 12 characters.">
				<Input id="{uid}-reset" type="password" bind:value={resetPassword} autocomplete="new-password" />
			</Field>
			<Field label="New password again" for="{uid}-reset-again">
				<Input
					id="{uid}-reset-again"
					type="password"
					bind:value={resetConfirm}
					autocomplete="new-password"
				/>
			</Field>
			{#if resetFailure}
				<Text as="p" tone="danger" size="sm" role="alert">{resetFailure}</Text>
			{/if}
		</Stack>
	{/snippet}
	{#snippet footer()}
		<Button onclick={() => (resetOpen = false)} disabled={resetBusy}>Cancel</Button>
		<Button variant="primary" loading={resetBusy} onclick={saveReset}>Set password</Button>
	{/snippet}
</Modal>

<ConfirmModal
	bind:open={removeOpen}
	title="Remove this user?"
	message={removing
		? `${removing.name} can no longer sign in: their password, passkeys and tokens stop working. Activity can undo this.`
		: ''}
	confirmLabel="Remove"
	tone="danger"
	onconfirm={removeUser}
/>

<ConfirmModal
	bind:open={revokeOpen}
	title="Revoke this token?"
	message={revoking
		? `Anything using the token ${revoking.token} of ${revoking.user} stops working. Activity can undo this.`
		: ''}
	confirmLabel="Revoke"
	tone="danger"
	onconfirm={revokeToken}
/>

<style>
	.narrow {
		max-inline-size: 56rem;
	}
	.rows > li {
		display: flex;
		flex-direction: column;
		gap: var(--sp-2);
		padding-block: var(--sp-3);
	}
	.rows > li + li {
		border-block-start: 1px solid var(--border);
	}
	.head,
	.line,
	.token,
	.secret,
	.actions {
		display: flex;
		align-items: center;
		gap: var(--sp-2);
	}
	.head {
		gap: var(--sp-3);
		flex-wrap: wrap;
	}
	.head > .grow {
		flex-basis: 12rem;
	}
	.line {
		flex-wrap: wrap;
	}
	.grow {
		display: flex;
		flex-direction: column;
		flex: 1;
		min-inline-size: 0;
	}
	.tokens {
		display: flex;
		flex-direction: column;
		gap: var(--sp-1);
		padding-inline-start: var(--sp-4);
	}
	.members {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-2) var(--sp-4);
	}
	.secret {
		padding: var(--sp-2) var(--sp-3);
		border-radius: var(--radius-md);
		background: var(--bg-elevated);
	}
	.actions {
		flex-wrap: wrap;
	}
</style>

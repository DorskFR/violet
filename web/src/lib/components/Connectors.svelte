<script lang="ts">
	import {
		Badge,
		Button,
		Checkbox,
		ConfirmModal,
		Field,
		Input,
		Modal,
		SectionHeader,
		Select,
		Stack,
		Text,
		Timestamp,
		toasts,
	} from '@dorsk/tsumikit';
	import {
		api,
		type Connector,
		type ConnectorInput,
		type ConnectorItem,
		type ConnectorKind,
		type ConnectorsStatus,
		errorMessage,
	} from '#lib/api';
	import {
		AUTH_LABEL,
		FREQUENCIES,
		INTERVAL_UNITS,
		offsetText,
		optionDefaults,
		optionsOf,
		runSummary,
		type ScheduleParts,
		scheduleOf,
		scheduleParts,
		scheduleText,
		WEEKDAYS,
	} from '#lib/connectors';
	import { session } from '#lib/session.svelte.ts';
	import { parseNames } from '#lib/users';

	const POLL_MS = 5000;
	const uid = $props.id();

	let status = $state<ConnectorsStatus | null>(null);
	let failure = $state('');
	let kinds = $state<ConnectorKind[]>([]);
	let knownAreas = $state<string[]>([]);
	let starting = $state<string[]>([]);
	let connecting = $state<string[]>([]);
	let refetching = $state<number[]>([]);

	let codeOpen = $state(false);
	let codeFor = $state('');
	let prompt = $state('');
	let code = $state('');
	let codeBusy = $state(false);
	let codeFailure = $state('');

	const authTone = { ready: 'ok', needs_code: 'warn', needs_login: 'warn' } as const;
	const itemTone = { ingested: 'ok', skipped: undefined, failed: 'danger' } as const;
	const itemLabel = { ingested: 'fetched', skipped: 'skipped', failed: 'failed' } as const;

	let formOpen = $state(false);
	let editing = $state<Connector | null>(null);
	let name = $state('');
	let kindName = $state('');
	let username = $state('');
	let password = $state('');
	let schedule = $state<ScheduleParts>(scheduleParts(''));
	let utcOffset = $state('');
	let area = $state('');
	let tags = $state('');
	let options = $state<Record<string, unknown>>({});
	let formBusy = $state(false);
	let formFailure = $state('');

	let removing = $state<Connector | null>(null);
	let removeOpen = $state(false);

	const kind = $derived(kinds.find((k) => k.kind === kindName));
	const kindChoices = $derived(kinds.map((k) => ({ value: k.kind, label: k.label })));
	const kindLabel = (kind: string) => kinds.find((k) => k.kind === kind)?.label ?? kind;

	async function load() {
		try {
			status = await api.connectors();
			failure = '';
		} catch (e) {
			failure = errorMessage(e, 'The connectors could not be loaded.');
		}
	}

	async function loadChoices() {
		try {
			kinds = (await api.connectorKinds()).kinds;
		} catch {
			kinds = [];
		}
		try {
			knownAreas = (await api.facets()).areas.map((f) => f.value);
		} catch {
			knownAreas = [];
		}
	}

	$effect(() => {
		loadChoices();
		load();
		const timer = setInterval(() => {
			if (document.visibilityState === 'visible' && !codeOpen && !formOpen) load();
		}, POLL_MS);
		return () => clearInterval(timer);
	});

	function openForm(connector: Connector | null) {
		editing = connector;
		name = connector?.name ?? '';
		kindName = connector?.kind ?? kinds[0]?.kind ?? '';
		username = connector?.username ?? '';
		password = '';
		schedule = scheduleParts(connector?.schedule ?? 'daily 09:00');
		utcOffset = connector?.utc_offset ?? offsetText(new Date().getTimezoneOffset());
		area = connector?.area ?? '';
		tags = connector?.tags.join(', ') ?? '';
		options = { ...optionDefaults(kinds.find((k) => k.kind === kindName)), ...connector?.options };
		formFailure = '';
		formOpen = true;
	}

	function pickKind(picked: string) {
		kindName = picked;
		options = optionDefaults(kinds.find((k) => k.kind === picked));
	}

	function chosen(option: string): string[] {
		const value = options[option];
		return Array.isArray(value) ? value : [];
	}

	function toggle(option: string, value: string, on: boolean) {
		const rest = chosen(option).filter((v) => v !== value);
		options[option] = on ? [...rest, value] : rest;
	}

	async function saveForm() {
		const input: ConnectorInput = {
			username: username.trim(),
			password: password || null,
			schedule: scheduleOf(schedule),
			utc_offset: utcOffset.trim(),
			area: area.trim() || null,
			tags: parseNames(tags),
			options: kind ? optionsOf(kind, options) : (editing?.options ?? {}),
		};
		formBusy = true;
		formFailure = '';
		try {
			if (editing) {
				const saved = await api.saveConnector(editing.name, input);
				toasts.ok(
					saved.auth === 'needs_login' && editing.auth !== 'needs_login'
						? `${saved.name} is saved and needs to connect again`
						: `${saved.name} is saved`,
				);
			} else {
				const added = await api.addConnector({ ...input, name: name.trim(), kind: kindName });
				toasts.ok(`${added.name} is added; connect it to sign in`);
			}
			formOpen = false;
			await load();
		} catch (e) {
			formFailure = errorMessage(e, 'The connector could not be saved.');
		} finally {
			formBusy = false;
		}
	}

	async function remove() {
		if (!removing) return;
		const gone = removing.name;
		try {
			await api.removeConnector(gone);
			toasts.ok(`${gone} is removed`);
			await load();
		} catch (e) {
			toasts.error(errorMessage(e, 'The connector could not be removed.'));
		}
	}

	async function run(connector: Connector) {
		starting = [...starting, connector.name];
		try {
			await api.runConnector(connector.name);
			toasts.ok(`${connector.name} is checking for new documents`);
			await load();
		} catch (e) {
			toasts.error(errorMessage(e, 'The run could not be started.'));
		} finally {
			starting = starting.filter((name) => name !== connector.name);
		}
	}

	function askCode(name: string, sitePrompt: string | null) {
		codeFor = name;
		prompt = sitePrompt ?? '';
		code = '';
		codeFailure = '';
		codeOpen = true;
	}

	async function connect(connector: Connector) {
		connecting = [...connecting, connector.name];
		try {
			const answer = await api.connectConnector(connector.name);
			if (answer.auth === 'needs_code') askCode(connector.name, answer.prompt);
			else toasts.ok(`${connector.name} is signed in`);
			await load();
		} catch (e) {
			toasts.error(errorMessage(e, 'The sign-in failed.'));
		} finally {
			connecting = connecting.filter((name) => name !== connector.name);
		}
	}

	async function submitCode() {
		const entered = code.trim();
		if (!entered) {
			codeFailure = 'Enter the code the site sent.';
			return;
		}
		codeBusy = true;
		codeFailure = '';
		try {
			await api.submitConnectorCode(codeFor, entered);
			codeOpen = false;
			toasts.ok(`${codeFor} is signed in and checks for documents now`);
			await load();
		} catch (e) {
			codeFailure = errorMessage(e, 'The code was not accepted.');
		} finally {
			codeBusy = false;
		}
	}

	async function refetch(item: ConnectorItem) {
		refetching = [...refetching, item.id];
		try {
			await api.refetchConnectorItem(item.id);
			toasts.ok(`${item.connector} fetches it again now`);
			await load();
		} catch (e) {
			toasts.error(errorMessage(e, 'The document could not be fetched again.'));
		} finally {
			refetching = refetching.filter((id) => id !== item.id);
		}
	}
</script>

<Stack gap="var(--sp-2)">
	<SectionHeader
		title="Connectors"
		level={2}
		size="sm"
		count={status?.connectors.length || undefined}
	>
		{#snippet actions()}
			<Button size="sm" disabled={kinds.length === 0} onclick={() => openForm(null)}>
				Add connector
			</Button>
		{/snippet}
	</SectionHeader>
	<Text as="p" tone="muted" size="sm">
		Connectors sign in to a website and bring its new documents into the inbox on a schedule. The
		documents of a connector are filed for the user who added it.
	</Text>
	{#if failure && !status}
		<Text as="p" tone="danger" size="sm" role="alert">{failure}</Text>
	{:else if status}
		{#if status.connectors.length > 0}
			<ul class="plain-list rows">
				{#each status.connectors as connector (connector.name)}
					<li>
						<div class="grow">
							<div class="line">
								<Text weight="medium" wrap="anywhere">{connector.name}</Text>
								<Badge size="xs" tone="muted">{kindLabel(connector.kind)}</Badge>
								<Badge size="xs" tone={authTone[connector.auth]}>{AUTH_LABEL[connector.auth]}</Badge>
								{#if connector.owner && connector.owner !== session.me?.name}
									<Badge size="xs" tone="muted">of {connector.owner}</Badge>
								{/if}
							</div>
							<Text tone="muted" size="sm" wrap="anywhere">
								{connector.username} · {scheduleText(connector.schedule, connector.utc_offset)}
							</Text>
							<Text tone="muted" size="sm">
								{#if connector.running}
									Checking now…
								{:else if connector.next_run}
									Next run <Timestamp value={connector.next_run} mode="relative" size="sm" tone="muted" />
								{:else}
									No run until it is signed in
								{/if}
								·
								{#if connector.last_run}
									last run <Timestamp value={connector.last_run} mode="relative" size="sm" tone="muted" />
								{:else}
									never ran
								{/if}
							</Text>
							<Text tone="muted" size="sm">{runSummary(connector)}</Text>
							{#if connector.auth === 'needs_code' && connector.prompt}
								<Text tone="warn" size="sm" wrap="anywhere">{connector.prompt}</Text>
							{:else if connector.last_error}
								<Text tone="danger" size="sm" wrap="anywhere">{connector.last_error}</Text>
							{/if}
						</div>
						<div class="buttons">
							{#if connector.auth === 'needs_code'}
								<Button
									size="sm"
									onclick={() => askCode(connector.name, connector.prompt)}
								>
									Enter code
								</Button>
							{/if}
							{#if connector.auth !== 'ready'}
								<Button
									size="sm"
									loading={connecting.includes(connector.name)}
									onclick={() => connect(connector)}
								>
									{connector.auth === 'needs_code' ? 'Send a new code' : 'Connect'}
								</Button>
							{/if}
							<Button
								size="sm"
								disabled={connector.auth === 'needs_code'}
								loading={connector.running || starting.includes(connector.name)}
								onclick={() => run(connector)}
							>
								Run now
							</Button>
							<Button
								size="sm"
								variant="ghost"
								aria-label="Edit {connector.name}"
								onclick={() => openForm(connector)}
							>
								Edit
							</Button>
							<Button
								size="sm"
								variant="ghost"
								aria-label="Remove {connector.name}"
								onclick={() => {
									removing = connector;
									removeOpen = true;
								}}
							>
								Remove
							</Button>
						</div>
					</li>
				{/each}
			</ul>
		{:else}
			<Text as="p" tone="muted" size="sm">No connector is added yet.</Text>
		{/if}

		{#if status.items.length > 0}
			<SectionHeader title="Recent documents" level={3} size="sm" count={status.items.length} />
			<ul class="plain-list rows">
				{#each status.items as item (item.id)}
					<li>
						<div class="grow">
							<Text weight="medium" wrap="anywhere">{item.title || item.remote_id}</Text>
							<Text tone="muted" size="sm" wrap="anywhere">
								{item.connector}{item.date ? ` · ${item.date}` : ''} ·
								<Timestamp value={item.fetched_at} mode="relative" size="sm" tone="muted" />
							</Text>
							{#if item.error}
								<Text tone="danger" size="sm" wrap="anywhere">{item.error}</Text>
							{:else if item.detail}
								<Text tone="muted" size="sm" wrap="anywhere">{item.detail}</Text>
							{/if}
						</div>
						<div class="buttons">
							<Badge tone={itemTone[item.status]} size="sm">{itemLabel[item.status]}</Badge>
							<Button
								size="sm"
								loading={refetching.includes(item.id)}
								aria-label="Fetch {item.title || item.remote_id} again"
								onclick={() => refetch(item)}
							>
								Fetch again
							</Button>
						</div>
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</Stack>

<Modal
	bind:open={formOpen}
	title={editing ? `Edit ${editing.name}` : 'Add connector'}
	busy={formBusy}
>
	{#snippet body()}
		<Stack gap="var(--sp-3)">
			{#if !editing}
				<Field
					label="Name"
					for="{uid}-name"
					hint="Letters, digits, '-', '_' or '.'. It cannot be changed later."
				>
					<Input id="{uid}-name" bind:value={name} placeholder="payslips" autocomplete="off" />
				</Field>
				<Field label="Site" for="{uid}-kind">
					<Select
						id="{uid}-kind"
						value={kindName}
						options={kindChoices}
						onchange={(e) => pickKind(e.currentTarget.value)}
					/>
				</Field>
			{/if}
			<Field label="Username" for="{uid}-username" hint="What you sign in to the site with.">
				<Input id="{uid}-username" bind:value={username} autocomplete="off" />
			</Field>
			<Field
				label="Password"
				for="{uid}-password"
				hint={editing
					? 'A password is set and is never shown. Leave this empty to keep it.'
					: 'Kept on the server and never shown again.'}
			>
				<Input
					id="{uid}-password"
					type="password"
					bind:value={password}
					placeholder={editing ? 'Unchanged' : ''}
					autocomplete="new-password"
				/>
			</Field>
			{#if editing}
				<Text as="p" tone="muted" size="sm">
					A new username or password signs the connector out: it needs Connect again.
				</Text>
			{/if}

			<Field label="Runs" for="{uid}-every">
				<Select id="{uid}-every" bind:value={schedule.every} options={FREQUENCIES} />
			</Field>
			<div class="pair">
				{#if schedule.every === 'interval'}
					<Field label="Every" for="{uid}-count">
						<Input
							id="{uid}-count"
							type="number"
							min="1"
							inputmode="numeric"
							bind:value={schedule.count}
						/>
					</Field>
					<Field label="Unit" for="{uid}-unit">
						<Select id="{uid}-unit" bind:value={schedule.unit} options={INTERVAL_UNITS} />
					</Field>
				{:else}
					{#if schedule.every === 'weekly'}
						<Field label="Day" for="{uid}-weekday">
							<Select id="{uid}-weekday" bind:value={schedule.weekday} options={WEEKDAYS} />
						</Field>
					{:else if schedule.every === 'monthly'}
						<Field label="Day of the month" for="{uid}-month-day" hint="1 to 28.">
							<Input
								id="{uid}-month-day"
								type="number"
								min="1"
								max="28"
								inputmode="numeric"
								bind:value={schedule.monthDay}
							/>
						</Field>
					{/if}
					<Field label="At" for="{uid}-at">
						<Input id="{uid}-at" type="time" bind:value={schedule.at} />
					</Field>
					<Field label="UTC offset" for="{uid}-offset" hint="Of that time, such as +09:00.">
						<Input id="{uid}-offset" bind:value={utcOffset} mono autocomplete="off" />
					</Field>
				{/if}
			</div>

			{#each kind?.options ?? [] as option (option.name)}
				{#if option.type === 'choices'}
					<Field label={option.label} hint={option.help ?? undefined}>
						<div class="choices" role="group" aria-label={option.label}>
							{#each option.choices as choice (choice.value)}
								<Checkbox
									label={choice.label}
									checked={chosen(option.name).includes(choice.value)}
									onchange={(e) => toggle(option.name, choice.value, e.currentTarget.checked)}
								/>
							{/each}
						</div>
					</Field>
				{:else}
					<Field
						label={option.label}
						for="{uid}-option-{option.name}"
						hint={option.help ?? undefined}
					>
						<Input
							id="{uid}-option-{option.name}"
							type="date"
							value={typeof options[option.name] === 'string' ? options[option.name] : ''}
							onchange={(e) => (options[option.name] = e.currentTarget.value)}
						/>
					</Field>
				{/if}
			{/each}

			<Field
				label="Area"
				for="{uid}-area"
				hint="Where its documents are filed. Empty: wherever each document belongs."
			>
				<Input id="{uid}-area" bind:value={area} list="{uid}-areas" autocomplete="off" />
			</Field>
			<datalist id="{uid}-areas">
				{#each knownAreas as known (known)}<option value={known}></option>{/each}
			</datalist>
			<Field label="Tags" for="{uid}-tags" hint="Added to each document, separated by commas.">
				<Input id="{uid}-tags" bind:value={tags} autocomplete="off" />
			</Field>
			{#if formFailure}
				<Text as="p" tone="danger" size="sm" role="alert">{formFailure}</Text>
			{/if}
		</Stack>
	{/snippet}
	{#snippet footer()}
		<Button onclick={() => (formOpen = false)} disabled={formBusy}>Cancel</Button>
		<Button variant="primary" loading={formBusy} onclick={saveForm}>
			{editing ? 'Save' : 'Add connector'}
		</Button>
	{/snippet}
</Modal>

<ConfirmModal
	bind:open={removeOpen}
	title="Remove this connector?"
	message={removing
		? `${removing.name} stops fetching, and its username, password and saved sign-in are deleted. The documents it fetched stay.`
		: ''}
	confirmLabel="Remove"
	tone="danger"
	onconfirm={remove}
/>

<Modal bind:open={codeOpen} title="Code for {codeFor}" busy={codeBusy}>
	{#snippet body()}
		<Stack gap="var(--sp-3)">
			<Text as="p" tone="muted" size="sm">
				{prompt || 'The site sent a code to finish the sign-in.'}
			</Text>
			<Text as="p" tone="muted" size="sm">The code can be entered for 15 minutes.</Text>
			<Field label="Code" for="{uid}-code">
				<Input
					id="{uid}-code"
					bind:value={code}
					mono
					inputmode="numeric"
					autocomplete="one-time-code"
					maxlength={64}
					invalid={codeFailure !== ''}
					onenter={submitCode}
				/>
			</Field>
			{#if codeFailure}
				<Text as="p" tone="danger" size="sm" role="alert">{codeFailure}</Text>
			{/if}
		</Stack>
	{/snippet}
	{#snippet footer()}
		<Button onclick={() => (codeOpen = false)} disabled={codeBusy}>Cancel</Button>
		<Button variant="primary" loading={codeBusy} onclick={submitCode}>Sign in</Button>
	{/snippet}
</Modal>

<style>
	.rows > li {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--sp-2) var(--sp-3);
		padding-block: var(--sp-3);
	}
	.rows > li + li {
		border-block-start: 1px solid var(--border);
	}
	.grow {
		display: flex;
		flex-direction: column;
		flex: 1 1 16rem;
		min-inline-size: 0;
	}
	.pair {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(9rem, 1fr));
		gap: var(--sp-3);
	}
	.choices {
		display: flex;
		flex-wrap: wrap;
		gap: var(--sp-2) var(--sp-4);
	}
	.line,
	.buttons {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--sp-2);
	}
</style>

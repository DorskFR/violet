<script lang="ts">
	import {
		Badge,
		Button,
		Field,
		Input,
		Modal,
		SectionHeader,
		Stack,
		Text,
		Timestamp,
		toasts,
	} from '@dorsk/tsumikit';
	import {
		api,
		type Connector,
		type ConnectorItem,
		type ConnectorsStatus,
		errorMessage,
	} from '#lib/api';
	import { AUTH_LABEL, runSummary, scheduleText } from '#lib/connectors';

	const POLL_MS = 5000;
	const uid = $props.id();

	let status = $state<ConnectorsStatus | null>(null);
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
	const shown = $derived(status !== null && (status.enabled || status.items.length > 0));

	async function load() {
		try {
			status = await api.connectors();
		} catch {
			status = null;
		}
	}

	$effect(() => {
		load();
		const timer = setInterval(() => {
			if (document.visibilityState === 'visible' && !codeOpen) load();
		}, POLL_MS);
		return () => clearInterval(timer);
	});

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

{#if status && shown}
	<Stack gap="var(--sp-2)">
		<SectionHeader
			title="Connectors"
			level={2}
			size="sm"
			count={status.connectors.length || undefined}
		/>
		<Text as="p" tone="muted" size="sm">
			Connectors sign in to a website and bring its new documents into the inbox on a schedule.
		</Text>
		{#if status.connectors.length > 0}
			<ul class="plain-list rows">
				{#each status.connectors as connector (connector.name)}
					<li>
						<div class="grow">
							<div class="line">
								<Text weight="medium" wrap="anywhere">{connector.name}</Text>
								<Badge size="xs" tone="muted">{connector.kind}</Badge>
								<Badge size="xs" tone={authTone[connector.auth]}>{AUTH_LABEL[connector.auth]}</Badge>
							</div>
							<Text tone="muted" size="sm" wrap="anywhere">
								{scheduleText(connector.schedule, status.utc_offset)}
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
						</div>
					</li>
				{/each}
			</ul>
		{:else}
			<Text as="p" tone="muted" size="sm">
				Connectors are off; the documents below were fetched earlier.
			</Text>
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
	</Stack>
{/if}

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
	.line,
	.buttons {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--sp-2);
	}
</style>

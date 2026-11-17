<script lang="ts">
	import {
		AutoGrid,
		Badge,
		Callout,
		Dropzone,
		EmptyState,
		Heading,
		Metric,
		SectionHeader,
		Skeleton,
		Stack,
		Text,
		Timestamp,
		toasts,
	} from '@dorsk/tsumikit';
	import { api, errorMessage, type Status } from '#lib/api';

	const POLL_MS = 5000;

	let status = $state<Status | null>(null);
	let error = $state('');
	let uploading = $state(0);

	async function load() {
		try {
			status = await api.status();
			error = '';
		} catch (e) {
			error = errorMessage(e, 'The status could not be loaded.');
		}
	}

	$effect(() => {
		load();
		const timer = setInterval(() => {
			if (document.visibilityState === 'visible') load();
		}, POLL_MS);
		return () => clearInterval(timer);
	});

	async function upload(files: File[]) {
		if (files.length === 0) return;
		uploading += files.length;
		try {
			const { queued } = await api.upload(files);
			toasts.ok(queued === 1 ? '1 file queued for reading' : `${queued} files queued for reading`);
			await load();
		} catch (e) {
			toasts.error(
				`${errorMessage(e, 'The upload failed.')} Nothing was added; drop the files again to retry.`,
			);
		} finally {
			uploading -= files.length;
		}
	}

	const models = $derived(
		status
			? [
					{ name: 'Vision model', role: 'reads scans and photos', up: status.model.vision },
					{ name: 'Text model', role: 'names and files documents', up: status.model.text },
				]
			: [],
	);
</script>

<svelte:head>
	<title>Inbox · violet</title>
</svelte:head>

<div class="page narrow">
	<Heading level={1} size="lg">Inbox</Heading>

	<Dropzone
		onfiles={upload}
		multiple
		label={uploading ? `Uploading ${uploading} ${uploading === 1 ? 'file' : 'files'}…` : 'Drop files here, or choose them'}
		hint="Scans, PDFs, photos, office files and emails. violet reads, names and files each one."
		disabled={uploading > 0}
	/>

	{#if error && !status}
		<EmptyState
			tone="danger"
			icon="alert-circle"
			title="The status did not load"
			description="{error} Check that the violet server is up, then try again."
			actionLabel="Try again"
			onAction={load}
		/>
	{:else if !status}
		<AutoGrid min="8rem">
			{#each { length: 5 } as _, i (i)}<Skeleton height="4.5rem" />{/each}
		</AutoGrid>
	{:else}
		{#if error}
			<Callout tone="warn" title="Showing the last known status">{error}</Callout>
		{/if}
		<AutoGrid min="8rem">
			<Metric label="In the inbox" value={status.inbox} size="sm" />
			<Metric label="Waiting" value={status.jobs.pending} size="sm" />
			<Metric
				label="Being read"
				value={status.jobs.running}
				size="sm"
				tone={status.jobs.running ? 'info' : undefined}
			/>
			<Metric label="Done" value={status.jobs.done} size="sm" />
			<Metric
				label="Failed"
				value={status.jobs.failed}
				size="sm"
				tone={status.jobs.failed ? 'danger' : undefined}
				tintValue={status.jobs.failed > 0}
			/>
		</AutoGrid>

		<Stack gap="var(--sp-2)">
			<SectionHeader title="Models" level={2} size="sm" />
			<ul class="plain-list rows">
				{#each models as m (m.name)}
					<li>
						<div class="grow">
							<Text weight="medium">{m.name}</Text>
							<Text tone="muted" size="sm">{m.role}</Text>
						</div>
						<Badge tone={m.up ? 'ok' : 'danger'} icon={m.up ? 'check-circle' : 'x-circle'} size="sm">
							{m.up ? 'reachable' : 'unreachable'}
						</Badge>
					</li>
				{/each}
			</ul>
			{#if models.some((m) => !m.up)}
				<Text as="p" tone="muted" size="sm">
					Files wait in the inbox while a model is unreachable and are read once it answers again.
				</Text>
			{/if}
		</Stack>

		<Stack gap="var(--sp-2)">
			<SectionHeader title="Failed" level={2} size="sm" count={status.failed.length || undefined} />
			{#if status.failed.length === 0}
				<Text as="p" tone="muted" size="sm">No file has failed. Files that cannot be read are listed here with the reason.</Text>
			{:else}
				<ul class="plain-list rows">
					{#each status.failed as job (job.id)}
						<li class="job">
							<div class="grow">
								<Text weight="medium" wrap="anywhere">{job.payload.path ?? job.key.slice(0, 12)}</Text>
								<Text tone="danger" size="sm" wrap="anywhere">{job.error ?? 'No reason was recorded.'}</Text>
								<Text tone="muted" size="sm">
									{job.attempts === 1 ? '1 attempt' : `${job.attempts} attempts`}, last
									<Timestamp value={job.updated_at} mode="relative" size="sm" tone="muted" />
								</Text>
							</div>
						</li>
					{/each}
				</ul>
				<Text as="p" tone="muted" size="sm">
					A failed file stays in the inbox. Dropping it here again queues another attempt.
				</Text>
			{/if}
		</Stack>

		<Text tone="faint" size="xs">violet {status.version}</Text>
	{/if}
</div>

<style>
	.narrow {
		max-inline-size: 56rem;
	}
	.rows > li {
		display: flex;
		align-items: center;
		gap: var(--sp-3);
		padding-block: var(--sp-2);
	}
	.rows > li + li {
		border-block-start: 1px solid var(--border);
	}
	.grow {
		display: flex;
		flex-direction: column;
		flex: 1;
		min-inline-size: 0;
	}
</style>

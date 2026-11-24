<script lang="ts">
  import { onMount } from 'svelte'
  import { request } from '../lib/api'
  let { allowWrites }: { allowWrites: boolean } = $props()
  interface Rule { id: string; name: string; enabled: boolean; log_paths: string[]; attempts: number; weight: number }
  interface Ban { network: string; reason: string; expires_at: string; hit_count: number; offense_count: number }
  interface Firewall {
    policy: {enabled: boolean; observe_only: boolean; allowlist: string[]; rules: Rule[]}
    snapshot: {active_ban_count: number; banned_ips?: unknown[]; banned_subnets?: unknown[]}
    bans: Ban[]
    health: unknown
    tuning: Record<string, number>
  }
  let status = $state<Firewall | null>(null)
  let capability = $state<{enforcement: boolean; reason: string} | null>(null)
  let loading = $state(false)
  let error = $state('')
  let filter = $state('')
  let controller: AbortController | undefined
  const bans = $derived(status?.bans.filter(b => (b.network + ' ' + b.reason).toLowerCase().includes(filter.toLowerCase())) ?? [])
  async function refresh() {
    controller?.abort(); const active = new AbortController(); controller = active
    loading = true; error = ''
    try { const [value, grant] = await Promise.all([request<Firewall>('/api/shared/firewall', { signal: active.signal }), request<{enforcement: boolean; reason: string}>('/api/shared/firewall/capability', {signal: active.signal})]); if (!active.signal.aborted) { status = value; capability = grant; policyText = JSON.stringify(value.policy, null, 2) } }
    catch (failure) { if (!active.signal.aborted) error = failure instanceof Error ? failure.message : 'Ban Shield unavailable' }
    finally { if (!active.signal.aborted) loading = false }
  }
  let busy = $state(false)
  let actionMessage = $state('')
  let policyText = $state('')
  let network = $state('')
  let banNetwork = $state('')
  let seconds = $state(3600)
  let reason = $state('Manual observation')
  async function mutate(path: string, method: string, body?: unknown) {
    if (!allowWrites || busy) return
    busy = true; error = ''; actionMessage = ''
    try { await request('/api/shared/firewall' + path, { method, body: body === undefined ? undefined : JSON.stringify(body) }, 90_000); actionMessage = 'Operation completed'; await refresh() }
    catch (failure) { error = failure instanceof Error ? failure.message : String(failure) }
    finally { busy = false }
  }
  function savePolicy() {
    try { const policy = JSON.parse(policyText); if (policy.enabled && !policy.observe_only && (!capability?.enforcement || !confirm('Enable host firewall enforcement? Verify your management IP is allowlisted.'))) return; void mutate('', 'PUT', policy) }
    catch (failure) { error = String(failure) }
  }
  onMount(() => { void refresh(); return () => controller?.abort() })
</script>

<p class="policy-note">{capability?.reason ?? 'Checking helper authorization…'}. Observation does not block traffic. Enforcement manages only Maestro's dedicated nftables input table.</p>
{#if actionMessage}<p role="status">{actionMessage}</p>{/if}
{#if error}<p class="form-error" role="alert">{error}</p>{/if}
<section class="panel">
  <div class="resource-heading"><h2>Ban Shield performance</h2><button disabled={loading} onclick={refresh}>{loading ? 'Refreshing…' : 'Refresh'}</button></div>
  {#if status}<dl class="shared-stats"><div><dt>Active bans</dt><dd>{status.snapshot.active_ban_count}</dd></div><div><dt>IP bans</dt><dd>{status.snapshot.banned_ips?.length ?? '—'}</dd></div><div><dt>Subnet bans</dt><dd>{status.snapshot.banned_subnets?.length ?? '—'}</dd></div><div><dt>Detection rules</dt><dd>{status.policy.rules.length}</dd></div></dl>
  <details><summary>Engine health</summary><pre>{JSON.stringify(status.health, null, 2)}</pre></details>{/if}
</section>
{#if status}
  <section class="panel"><div class="resource-heading"><h2>Protection</h2><span class="project-state" class:running={status.policy.enabled}>{status.policy.enabled ? status.policy.observe_only ? 'Observe only' : 'Enabled' : 'Disabled'}</span></div>
    <h3>Allowlist</h3>{#if status.policy.allowlist.length}<ul>{#each status.policy.allowlist as network}<li class="mono">{network} <button disabled={!allowWrites || busy} onclick={() => { if (confirm('Remove allowlist entry?')) void mutate('/allowlist/' + encodeURIComponent(network), 'DELETE') }}>Remove</button></li>{/each}</ul>{:else}<p class="muted">No allowlist entries.</p>{/if}
    {#if allowWrites}<form onsubmit={e => { e.preventDefault(); void mutate('/allowlist', 'POST', {network}) }}><label>Network (CIDR) <input bind:value={network} placeholder="192.0.2.0/24" required /></label><button disabled={busy}>Add allowlist entry</button></form>
    <button disabled={busy} onclick={() => { if (status) void mutate('', 'PUT', {...status.policy, enabled: !status.policy.enabled, observe_only: true}) }}>{status.policy.enabled ? 'Disable' : 'Enable observation'}</button>
    <button disabled={busy || !capability?.enforcement || (status.policy.enabled && !status.policy.observe_only)} onclick={() => { if (status && confirm('Enable firewall enforcement? Verify your management IP is allowlisted.')) void mutate('', 'PUT', {...status.policy, enabled: true, observe_only: false}) }}>Enable enforcement</button>
    {#if status.policy.enabled && !status.policy.observe_only}<button disabled={busy} onclick={() => { if (status && confirm('Switch to observation and remove Maestro firewall hooks?')) void mutate('', 'PUT', {...status.policy, observe_only: true}) }}>Switch to observation</button>{/if}{/if}
  </section>
  <section class="panel"><div class="resource-heading"><h2>Active bans</h2><span>{status.bans.length} entries</span></div>
    <label for="ban-filter">Filter bans</label><input id="ban-filter" type="search" bind:value={filter} placeholder="Network or reason…" />
    <div class="shared-table"><table><thead><tr><th>Network</th><th>Reason</th><th>Hits</th><th>Expires</th><th>Actions</th></tr></thead><tbody>
      {#each bans as ban (ban.network)}<tr><td class="mono">{ban.network}</td><td>{ban.reason}</td><td>{ban.hit_count}</td><td>{new Date(ban.expires_at).toLocaleString()}</td><td><button disabled={!allowWrites || busy} onclick={() => { if (confirm('Unban and clear history for ' + ban.network + '?')) void mutate('/bans/' + encodeURIComponent(ban.network), 'DELETE') }}>Unban</button><button disabled={!allowWrites || busy} onclick={() => { if (confirm('Reset failure counts?')) void mutate('/reset-counts/' + encodeURIComponent(ban.network), 'POST') }}>Reset counts</button></td></tr>{/each}
    </tbody></table></div>{#if !bans.length}<p class="muted">No matching bans.</p>{/if}
    {#if allowWrites}<form onsubmit={e => { e.preventDefault(); void mutate('/bans', 'POST', {network: banNetwork, seconds, reason}) }}><label>Network (CIDR) <input bind:value={banNetwork} required /></label><label>Duration (seconds) <input type="number" min="60" bind:value={seconds} required /></label><label>Reason <input bind:value={reason} required /></label><button disabled={busy || !status.policy.enabled}>Record manual ban</button></form>{/if}
  </section>
  <section class="panel"><div class="resource-heading"><h2>Detection rules</h2></div>
    <div class="shared-table"><table><thead><tr><th>Rule</th><th>Log paths</th><th>Attempts</th><th>Weight</th><th>State</th></tr></thead><tbody>
      {#each status.policy.rules as rule (rule.id)}<tr><td>{rule.name}</td><td class="mono">{rule.log_paths.join(', ')}</td><td>{rule.attempts}</td><td>{rule.weight}</td><td>{rule.enabled ? 'Enabled' : 'Disabled'}</td></tr>{/each}
    </tbody></table></div>{#if !status.policy.rules.length}<p class="muted">No detection rules.</p>{/if}
    {#if allowWrites}<details><summary>Edit policy and rules (JSON)</summary><p>Set observe_only to false to enforce bans when the helper is authorized.</p><textarea aria-label="Firewall policy JSON" bind:value={policyText} rows="12"></textarea><button disabled={busy} onclick={savePolicy}>Save policy</button></details>{/if}
  </section>
  <section class="panel"><h2>Security tuning</h2><dl class="shared-stats">{#each Object.entries(status.tuning) as [key, value]}<div><dt>{key.replaceAll('_', ' ')}</dt><dd>{value}</dd></div>{/each}</dl></section>
{/if}
<style>
  .shared-table { overflow-x: auto; margin-top: 1rem; }
  table { width: 100%; border-collapse: collapse; text-align: left; }
  th, td { padding: .7rem; border-bottom: 1px solid var(--border); }
  th { color: var(--muted); font-size: .78rem; }
  .shared-stats { display: grid; grid-template-columns: repeat(auto-fit,minmax(10rem,1fr)); gap: 1rem; }
  .shared-stats dt { color: var(--muted); font-size: .85rem; }
  .shared-stats dd { margin: .25rem 0 0; }
  pre { max-height: 15rem; overflow: auto; white-space: pre-wrap; }
</style>

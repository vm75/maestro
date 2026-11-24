<script lang="ts">
  import { onMount } from 'svelte'
  import { request } from '../lib/api'
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
  let loading = $state(false)
  let error = $state('')
  let filter = $state('')
  let controller: AbortController | undefined
  const bans = $derived(status?.bans.filter(b => (b.network + ' ' + b.reason).toLowerCase().includes(filter.toLowerCase())) ?? [])
  async function refresh() {
    controller?.abort(); const active = new AbortController(); controller = active
    loading = true; error = ''
    try { const value = await request<Firewall>('/api/shared/firewall', { signal: active.signal }); if (!active.signal.aborted) status = value }
    catch (failure) { if (!active.signal.aborted) error = failure instanceof Error ? failure.message : 'Ban Shield unavailable' }
    finally { if (!active.signal.aborted) loading = false }
  }
  onMount(() => { void refresh(); return () => controller?.abort() })
</script>

<p class="policy-note">This page currently displays Ban Shield status and configuration. Controls for enforcement, allowlists, detection rules and bans have not yet been implemented in the Linux page.</p>
{#if error}<p class="form-error" role="alert">{error}</p>{/if}
<section class="panel">
  <div class="resource-heading"><h2>Ban Shield performance</h2><button disabled={loading} onclick={refresh}>{loading ? 'Refreshing…' : 'Refresh'}</button></div>
  {#if status}<dl class="shared-stats"><div><dt>Active bans</dt><dd>{status.snapshot.active_ban_count}</dd></div><div><dt>IP bans</dt><dd>{status.snapshot.banned_ips?.length ?? '—'}</dd></div><div><dt>Subnet bans</dt><dd>{status.snapshot.banned_subnets?.length ?? '—'}</dd></div><div><dt>Detection rules</dt><dd>{status.policy.rules.length}</dd></div></dl>
  <details><summary>Engine health</summary><pre>{JSON.stringify(status.health, null, 2)}</pre></details>{/if}
</section>
{#if status}
  <section class="panel"><div class="resource-heading"><h2>Protection</h2><span class="project-state" class:running={status.policy.enabled}>{status.policy.enabled ? status.policy.observe_only ? 'Observe only' : 'Enabled' : 'Disabled'}</span></div>
    <h3>Allowlist</h3>{#if status.policy.allowlist.length}<ul>{#each status.policy.allowlist as network}<li class="mono">{network}</li>{/each}</ul>{:else}<p class="muted">No allowlist entries.</p>{/if}
  </section>
  <section class="panel"><div class="resource-heading"><h2>Active bans</h2><span>{status.bans.length} entries</span></div>
    <label for="ban-filter">Filter bans</label><input id="ban-filter" type="search" bind:value={filter} placeholder="Network or reason…" />
    <div class="shared-table"><table><thead><tr><th>Network</th><th>Reason</th><th>Hits</th><th>Expires</th></tr></thead><tbody>
      {#each bans as ban (ban.network)}<tr><td class="mono">{ban.network}</td><td>{ban.reason}</td><td>{ban.hit_count}</td><td>{new Date(ban.expires_at).toLocaleString()}</td></tr>{/each}
    </tbody></table></div>{#if !bans.length}<p class="muted">No matching bans.</p>{/if}
  </section>
  <section class="panel"><div class="resource-heading"><h2>Detection rules</h2></div>
    <div class="shared-table"><table><thead><tr><th>Rule</th><th>Log paths</th><th>Attempts</th><th>Weight</th><th>State</th></tr></thead><tbody>
      {#each status.policy.rules as rule (rule.id)}<tr><td>{rule.name}</td><td class="mono">{rule.log_paths.join(', ')}</td><td>{rule.attempts}</td><td>{rule.weight}</td><td>{rule.enabled ? 'Enabled' : 'Disabled'}</td></tr>{/each}
    </tbody></table></div>{#if !status.policy.rules.length}<p class="muted">No detection rules.</p>{/if}
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

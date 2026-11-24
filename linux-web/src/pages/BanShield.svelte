<script lang="ts">
  import { onMount } from 'svelte'
  import { request } from '../lib/api'
  let { allowWrites }: { allowWrites: boolean } = $props()
  interface Rule { id?: string; name: string; enabled: boolean; log_paths: string[]; attempts: number; weight: number; pattern: string; ip_group: string; group_values: Record<string,string[]> }
  interface Ban { network: string; reason: string; expires_at: string; hit_count: number; offense_count: number; source: string }
  interface Firewall {
    policy: {enabled: boolean; observe_only: boolean; allowlist: string[]; rules: Rule[]; tuning?: Record<string,number>}
    snapshot: {active_ban_count: number; banned_ips?: unknown[]; banned_subnets?: unknown[]; rule_stats: [string, {match_count: number; ban_count: number}][]}
    bans: Ban[]
    health: {state: string; set_capacity: number; set_entries: number; dropped_line_count: number; command_timeout_count: number; error_count: number; last_error?: string}
    settings: {global_threshold: number}
    tuning: Record<string, number>
  }
  let status = $state<Firewall | null>(null)
  let capability = $state<{enforcement: boolean; reason: string} | null>(null)
  let loading = $state(false)
  let error = $state('')
  let filter = $state('')
  let controller: AbortController | undefined
  let banRows = $state<Ban[]>([])
  let bansOpen = $state(false)
  let bansLoading = $state(false)
  let bansError = $state('')
  let bansController: AbortController | undefined
  let banSort = $state<'network'|'reason'|'expires_at'|'hit_count'>('network')
  let banDirection = $state(1)
  function sortBans(key:typeof banSort){if(banSort===key)banDirection*=-1;else{banSort=key;banDirection=1}}
  const bans = $derived(banRows.filter(b=>(b.network+' '+b.reason).toLowerCase().includes(filter.toLowerCase())).sort((a,b)=>banDirection*(banSort==='hit_count'?a.hit_count-b.hit_count:String(a[banSort]).localeCompare(String(b[banSort]),undefined,{numeric:true}))))
  async function loadBans(){
    bansController?.abort();const active=new AbortController();bansController=active;bansLoading=true;bansError=''
    try{const value=await request<Ban[]>('/api/shared/firewall/bans',{signal:active.signal});if(!active.signal.aborted)banRows=value}
    catch(e){if(!active.signal.aborted)bansError=String(e)}
    finally{if(!active.signal.aborted)bansLoading=false}
  }
  function toggleBans(){bansOpen=!bansOpen;if(bansOpen)void loadBans();else{bansController?.abort();bansLoading=false;banRows=[]}}
  async function refresh() {
    controller?.abort(); const active = new AbortController(); controller = active
    loading = true; error = ''
    try { const [value, grant] = await Promise.all([request<Firewall>('/api/shared/firewall?include_bans=false', { signal: active.signal }), request<{enforcement: boolean; reason: string}>('/api/shared/firewall/capability', {signal: active.signal})]); if (!active.signal.aborted) { status = value; capability = grant; policyText = JSON.stringify(value.policy, null, 2) } }
    catch (failure) { if (!active.signal.aborted) error = failure instanceof Error ? failure.message : 'Ban Shield unavailable' }
    finally { if (!active.signal.aborted) {loading = false;if(bansOpen)await loadBans()} }
  }
  let busy = $state(false)
  let actionMessage = $state('')
  let policyText = $state('')
  let ruleDraft = $state<Rule | null>(null)
  let rulePaths = $state('')
  let tuningDraft = $state<Record<string,number> | null>(null)
  const strict = { ip_failures: 3, subnet_failures: 6, promote_after_banned_ips: 2, reputation_repromote_after_offenses: 1, score_retention_days: 60, reputation_retention_days: 365, subnet_promotion_window_days: 60, subnet_ban_days: 30, first_ban_days: 1, second_ban_days: 7, third_ban_days: 30, max_ban_days: 180 }
  const retentionWarning = $derived(!!status && (status.tuning.score_retention_days <= status.tuning.subnet_ban_days || status.tuning.subnet_promotion_window_days <= status.tuning.subnet_ban_days || status.tuning.reputation_retention_days <= status.tuning.max_ban_days))
  function editRule(rule?: Rule) {
    ruleDraft = rule ? JSON.parse(JSON.stringify(rule)) : {name:'Nginx authentication failure', enabled:true, log_paths:[], pattern:'client: (?P<ip>[0-9a-fA-F:.]+).*(?:authentication|password)', ip_group:'ip', group_values:{}, attempts:5, weight:1}
    rulePaths = ruleDraft!.log_paths.join('\n')
  }
  async function saveRule() {
    if (!status || !ruleDraft) return
    const next = {...ruleDraft, log_paths: rulePaths.split(/\r?\n/).map(v=>v.trim()).filter(Boolean)}
    if (await mutate('/rules' + (next.id ? '/' + encodeURIComponent(next.id) : ''), next.id ? 'PUT' : 'POST', next)) ruleDraft = null
  }
  async function saveTuning() {
    if (!status || !tuningDraft) return
    const t = tuningDraft
    if (Object.keys(strict).some(k => !Number.isInteger(t[k]) || t[k] < 1) || t.promote_after_banned_ips < 2 || t.score_retention_days <= t.subnet_ban_days || t.subnet_promotion_window_days <= t.subnet_ban_days || t.reputation_retention_days <= t.max_ban_days || t.first_ban_days > t.second_ban_days || t.second_ban_days > t.third_ban_days || t.third_ban_days > t.max_ban_days || t.subnet_ban_days > t.max_ban_days) { error = 'Use positive integers, increasing ban durations, at least two distinct IPs, and retention longer than the corresponding ban.'; return }
    if (await mutate('', 'PUT', {...status.policy,tuning:t})) tuningDraft = null
  }
  let network = $state('')
  let banNetwork = $state('')
  let seconds = $state(3600)
  let reason = $state('Manual observation')
  async function mutate(path: string, method: string, body?: unknown): Promise<boolean> {
    if (!allowWrites || busy) return false
    busy = true; error = ''; actionMessage = ''
    try { const result = await request<{message?:string}>('/api/shared/firewall' + path, { method, body: body === undefined ? undefined : JSON.stringify(body) }, 90_000); actionMessage = result.message || 'Operation completed'; await refresh(); return true }
    catch (failure) { error = failure instanceof Error ? failure.message : String(failure); return false }
    finally { busy = false }
  }
  function savePolicy() {
    try { const policy = JSON.parse(policyText); if (policy.enabled && !policy.observe_only && (!capability?.enforcement || !confirm('Enable host firewall enforcement? Verify your management IP is allowlisted.'))) return; void mutate('', 'PUT', policy) }
    catch (failure) { error = String(failure) }
  }
  onMount(() => { void refresh(); return () => {controller?.abort();bansController?.abort()} })
</script>

<p class="policy-note">{capability?.reason ?? 'Checking helper authorization…'}. Observation does not block traffic. Enforcement manages only Maestro's dedicated nftables input table.</p>
{#if actionMessage}<p role="status">{actionMessage}</p>{/if}
{#if error}<p class="form-error" role="alert">{error}</p>{/if}
<section class="panel">
  <div class="resource-heading"><h2>Ban Shield performance</h2><button disabled={loading} onclick={refresh}>{loading ? 'Refreshing…' : 'Refresh'}</button></div>
  {#if status}<dl class="shared-stats"><div><dt>Active bans</dt><dd>{status.snapshot.active_ban_count}</dd></div><div><dt>IP bans</dt><dd>{status.snapshot.banned_ips?.length ?? '—'}</dd></div><div><dt>Subnet bans</dt><dd>{status.snapshot.banned_subnets?.length ?? '—'}</dd></div><div><dt>Detection rules</dt><dd>{status.policy.rules.length}</dd></div></dl>
  <p>Engine: {status.health.state} · {status.health.error_count || status.health.dropped_line_count || status.health.command_timeout_count || retentionWarning || status.health.last_error ? 'Needs attention' : 'Healthy'}</p>
  <p>Set usage: {status.health.set_capacity ? status.health.set_entries + ' / ' + status.health.set_capacity + ' (' + (100*status.health.set_entries/status.health.set_capacity).toFixed(2) + '%)' : 'Unavailable'}</p>
  <p>{status.health.dropped_line_count} dropped lines · {status.health.command_timeout_count} timeouts · {status.health.error_count} errors</p>
  {#if status.health.last_error}<p class="form-error">{status.health.last_error}</p>{/if}
  {#if retentionWarning}<p class="form-error">Retention is not long enough to preserve hardening across configured bans.</p>{/if}
  <details><summary>Engine health details</summary><pre>{JSON.stringify(status.health, null, 2)}</pre></details>{/if}
</section>
{#if status}
  <section class="panel"><div class="resource-heading"><h2>Protection</h2><span class="project-state" class:running={status.policy.enabled}>{status.policy.enabled ? status.policy.observe_only ? 'Observe only' : 'Enabled' : 'Disabled'}</span></div>
    <h3>Allowlist</h3>{#if status.policy.allowlist.length}<ul>{#each status.policy.allowlist as network}<li class="mono">{network} <button disabled={!allowWrites || busy} onclick={() => { if (confirm('Remove allowlist entry?')) void mutate('/allowlist/' + encodeURIComponent(network), 'DELETE') }}>Remove</button></li>{/each}</ul>{:else}<p class="muted">No allowlist entries.</p>{/if}
    {#if allowWrites}<form onsubmit={e => { e.preventDefault(); void mutate('/allowlist', 'POST', {network: network.trim().includes('/') ? network.trim() : network.trim() + (network.includes(':') ? '/128' : '/32')}) }}><label>Network (CIDR) <input bind:value={network} placeholder="192.0.2.0/24" required /></label><button disabled={busy}>Add allowlist entry</button></form>
    <button disabled={busy} onclick={() => { if (status) void mutate('', 'PUT', {...status.policy, enabled: !status.policy.enabled, observe_only: true}) }}>{status.policy.enabled ? 'Disable' : 'Enable observation'}</button>
    <button disabled={busy || !capability?.enforcement || (status.policy.enabled && !status.policy.observe_only)} onclick={() => { if (status && confirm('Enable firewall enforcement? Verify your management IP is allowlisted.')) void mutate('', 'PUT', {...status.policy, enabled: true, observe_only: false}) }}>Enable enforcement</button>
    {#if status.policy.enabled && !status.policy.observe_only}<button disabled={busy} onclick={() => { if (status && confirm('Switch to observation and remove Maestro firewall hooks?')) void mutate('', 'PUT', {...status.policy, observe_only: true}) }}>Switch to observation</button>{/if}{/if}
  </section>
  <section class="panel"><div class="resource-heading"><h2>Active bans</h2><span>{status.snapshot.active_ban_count} entries</span><button type="button" onclick={toggleBans}>{bansOpen?'Hide active bans':'Show active bans'}</button></div>
    {#if bansOpen}
    {#if bansLoading}<p role="status">Loading active bans…</p>{/if}{#if bansError}<p class="form-error" role="alert">{bansError}</p>{/if}
    <label for="ban-filter">Filter bans</label><input id="ban-filter" type="search" bind:value={filter} placeholder="Network or reason…" />
    <div class="shared-table"><table><thead><tr>{#each [{key:'network',label:'Network'},{key:'reason',label:'Reason'},{key:'hit_count',label:'Hits'},{key:'expires_at',label:'Expires'}] as col}<th aria-sort={banSort===col.key?(banDirection===1?'ascending':'descending'):'none'}><button type="button" onclick={()=>sortBans(col.key as typeof banSort)}>{col.label} {banSort===col.key?(banDirection===1?'↑':'↓'):'↕'}</button></th>{/each}<th>Actions</th></tr></thead><tbody>
      {#each bans as ban (ban.network)}<tr><td class="mono">{ban.network}</td><td>{ban.reason}<small>{ban.source} · offense {ban.offense_count}</small></td><td>{ban.hit_count}</td><td>{new Date(ban.expires_at).toLocaleString()}</td><td><button disabled={!allowWrites || busy} onclick={() => { if (confirm('Unban, clear history and add ' + ban.network + ' to the allowlist?')) void mutate('/bans/' + encodeURIComponent(ban.network), 'DELETE') }}>Unban + allow</button><button disabled={!allowWrites || busy} onclick={() => { if (confirm('Reset failure counts?')) void mutate('/reset-counts/' + encodeURIComponent(ban.network), 'POST') }}>Reset counts</button></td></tr>{/each}
    </tbody></table></div>{#if !bansLoading && !bansError && !bans.length}<p class="muted">No matching bans.</p>{/if}
    {/if}
    {#if allowWrites}<form onsubmit={e => { e.preventDefault(); void mutate('/bans', 'POST', {network: banNetwork, seconds, reason}) }}><label>Network (CIDR) <input bind:value={banNetwork} required /></label><label>Duration (seconds) <input type="number" min="60" bind:value={seconds} required /></label><label>Reason <input bind:value={reason} required /></label><button disabled={busy || !status.policy.enabled}>Record manual ban</button></form>{/if}
  </section>
  <section class="panel"><div class="resource-heading"><h2>Detection rules</h2>{#if allowWrites}<button disabled={busy} onclick={() => editRule()}>Add rule</button>{/if}</div>
    <div class="shared-table"><table><thead><tr><th>Rule</th><th>Log paths</th><th>Weight / threshold</th><th>Hits / bans</th><th>State</th><th>Actions</th></tr></thead><tbody>
      {#each status.policy.rules as rule (rule.id)}
        {@const stats = status.snapshot.rule_stats?.find(([name])=>name===rule.name)?.[1]}
        <tr><td>{rule.name}</td><td class="mono">{rule.log_paths.join(', ')}</td><td>{rule.weight} / {Math.ceil(status.settings.global_threshold / rule.weight)} hits from zero</td><td>{stats ? stats.match_count + ' / ' + stats.ban_count : '—'}</td><td>{rule.enabled ? 'Enabled' : 'Disabled'}</td><td><button disabled={!allowWrites || busy} onclick={() => editRule(rule)}>Edit</button><button disabled={!allowWrites || busy} onclick={() => { if (rule.id && confirm('Delete this detection rule?')) void mutate('/rules/'+encodeURIComponent(rule.id), 'DELETE') }}>Delete</button></td></tr>
      {/each}
    </tbody></table></div>{#if !status.policy.rules.length}<p class="muted">No detection rules.</p>{/if}
    {#if ruleDraft && allowWrites}
      <form onsubmit={e=>{e.preventDefault();void saveRule()}}><fieldset disabled={busy}>
        <h3>{ruleDraft.id ? 'Edit detection rule' : 'Add detection rule'}</h3>
        <label>Name <input required bind:value={ruleDraft.name}/></label><label><input type="checkbox" bind:checked={ruleDraft.enabled}/> Enabled</label>
        <label>Log paths (one per line)<textarea required rows="3" bind:value={rulePaths}></textarea></label>
        <label>Hybrid-DFA regex<textarea required rows="3" bind:value={ruleDraft.pattern}></textarea></label><p class="muted">Regex must provide the configured named IP capture.</p>
        <label>Weight <input type="number" min="1" required bind:value={ruleDraft.weight}/></label>
        <button>Save rule</button><button type="button" onclick={()=>ruleDraft=null}>Cancel</button>
      </fieldset></form>
    {/if}
    {#if allowWrites}<details><summary>Edit policy and rules (JSON)</summary><p>Set observe_only to false to enforce bans when the helper is authorized.</p><textarea aria-label="Firewall policy JSON" bind:value={policyText} rows="12"></textarea><button disabled={busy} onclick={savePolicy}>Save policy</button></details>{/if}
  </section>
  <section class="panel"><h2>Security tuning</h2><p>{status.policy.tuning ? 'Policy override active' : 'Using server defaults'}</p>
    {#if tuningDraft && allowWrites}
      <form onsubmit={e=>{e.preventDefault();void saveTuning()}}><fieldset disabled={busy}>
        <button type="button" onclick={()=>tuningDraft={...strict}}>Load strict preset</button>
        {#each Object.keys(strict) as key}<label>{key.replaceAll('_',' ')}<input type="number" required min={key==='promote_after_banned_ips'?2:1} bind:value={tuningDraft[key]}/></label>{/each}
        <button>Save security tuning</button><button type="button" onclick={()=>tuningDraft=null}>Cancel</button>
      </fieldset></form>
    {:else}<dl class="shared-stats">{#each Object.entries(status.tuning) as [key,value]}<div><dt>{key.replaceAll('_',' ')}</dt><dd>{value}</dd></div>{/each}</dl>
      {#if allowWrites}<button disabled={busy} onclick={()=>{if(status)tuningDraft={...status.tuning}}}>Edit security tuning</button>{/if}
    {/if}
  </section>
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

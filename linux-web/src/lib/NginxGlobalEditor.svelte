<script lang="ts">
  import { untrack } from 'svelte'
  import NginxFields from './NginxFields.svelte'
  import NginxPolicyEditor from './NginxPolicyEditor.svelte'
  import { clone, lines, sharedFields, listenerNames, type GlobalSettings, type Endpoint, type Usage, type Listener, type HttpAccess } from './nginx-config'
  let { initial, usage, disabled = false, onSave, onCancel }:
    {initial: GlobalSettings; usage: Usage; disabled?: boolean; onSave: (value: GlobalSettings) => Promise<void>; onCancel: () => void} = $props()
  // Capture the saved configuration once per editor mount, without resetting
  // unsaved edits when the parent refreshes.
  const snapshot = untrack(() => clone(initial))
  let draft = $state<GlobalSettings>(snapshot)
  let listenerName = $state('')
  let accessName = $state('')
  let limitName = $state('')
  let error = $state('')
  let http = $state<HttpAccess>(clone(snapshot.http_access ?? {mode:'renewal_only',window_seconds:3600,listener_profile:'legacy',wan:null,externally_managed_wan:false}))
  let manageHttp = $state(snapshot.http_access !== undefined)
  let allowed = $state(snapshot.allowed_ips.join('\n'))
  let suffixes = $state(snapshot.free_subdomains.join('\n'))
  const used = (kind: string, name: string) => usage[kind+':'+name] || []
  function add(kind: 'listener' | 'access' | 'limit') {
    error=''
    const name = (kind==='listener' ? listenerName : kind==='access' ? accessName : limitName).trim()
    if (!/^[A-Za-z0-9_]{1,32}$/.test(name) || ['legacy','public','deny','default','__proto__','constructor','prototype'].includes(name.toLowerCase())) { error='Choose a unique, non-reserved policy name (letters, numbers or underscore, up to 32 characters).';return }
    if (kind==='listener') { if (draft.listener_profiles[name]) {error='Listener already exists';return}; draft.listener_profiles[name]={https:[{address:'127.0.0.1',port:8444}],http:[],http_redirect:false}; listenerName='' }
    else if (kind==='access') {if (draft.access_policies[name]) {error='Access rule already exists';return};draft.access_policies[name]={allow:[]};accessName=''}
    else {if (draft.limit_policies[name]) {error='Limit policy already exists';return};draft.limit_policies[name]={rate:'30r/s',burst:60,nodelay:true,connections:30};limitName=''}
  }
  function remove(kind: 'listener'|'access'|'limit',name:string) {
    if (!confirm('Remove '+name+' from the draft? Existing references must be updated before saving.')) return
    if(kind==='listener') delete draft.listener_profiles[name]
    else if(kind==='access') delete draft.access_policies[name]
    else delete draft.limit_policies[name]
  }
  function rename(kind:'listener'|'access',old:string,input:HTMLInputElement) {
    const name=input.value.trim()
    const entries=kind==='listener'?draft.listener_profiles:draft.access_policies
    error=''
    if(name===old){input.value=old;return}
    if(!/^[A-Za-z0-9_]{1,32}$/.test(name)||['legacy','public','deny','default','__proto__','constructor','prototype'].includes(name.toLowerCase())||Object.hasOwn(entries,name)){error='Choose a unique, non-reserved policy name (letters, numbers or underscore, up to 32 characters).';input.value=old;return}
    if(used(kind,old).length&&!confirm('Rename '+old+'? Update all references before saving. Existing sites are not rewritten.')){input.value=old;return}
    if(kind==='listener'){draft.listener_profiles[name]=clone(draft.listener_profiles[old]);delete draft.listener_profiles[old]}
    else{draft.access_policies[name]=clone(draft.access_policies[old]);delete draft.access_policies[old]}
  }
  function endpoints(profile: Listener, protocol: 'http'|'https', values: Endpoint[]) {profile[protocol]=values}
  function number(value:string) {return value.trim() ? Number(value) : 0}
  async function save() {
    error=''
    try {
      const next=clone(draft)
      next.allowed_ips=lines(allowed);next.free_subdomains=lines(suffixes)
      if (http.wan && !confirm('Remove the old firmware WAN gate from these settings? Existing router-owned rules must be released by the administrator.')) return
      if(manageHttp) next.http_access={...http,wan:null}
      else delete next.http_access
      await onSave(next)
    } catch(e) {error=String(e)}
  }
</script>
<form onsubmit={e=>{e.preventDefault();void save()}}>
  <fieldset {disabled}>
    <h3>Public HTTP / certificate validation</h3>
    <p class="muted">Linux firewall and upstream NAT are externally managed. The router-specific firmware WAN gate is not available on Linux. Listener and HTTP lifecycle settings are identical.</p>
    {#if http.wan}<p class="form-error">This configuration contains a router-only WAN gate. Saving removes it; release existing router-owned rules before migration.</p>{/if}
    <label><input type="checkbox" bind:checked={manageHttp} disabled={snapshot.http_access !== undefined} /> Configure HTTP access lifecycle</label>
    {#if manageHttp}
      <div class="editor-grid">
        <label>HTTP mode<select bind:value={http.mode}><option value="off">Off</option><option value="renewal_only">Certificate validation only</option><option value="always">Always on with redirects</option></select></label>
        <label>Challenge listener<select bind:value={http.listener_profile}>{#each listenerNames(draft) as name}<option value={name}>{name}</option>{/each}{#if !listenerNames(draft).includes(http.listener_profile)}<option value={http.listener_profile}>Missing choice: {http.listener_profile}</option>{/if}</select></label>
        <label>Maximum validation window (seconds)<input type="number" min="60" max="86400" required value={http.window_seconds} oninput={e=>http.window_seconds=number(e.currentTarget.value)}/></label>
        <label><input type="checkbox" bind:checked={http.externally_managed_wan}/> WAN access managed externally</label>
      </div>
      {#if http.mode==='renewal_only'}<p class="muted">Validation-only mode closes managed public HTTP outside HTTP-01 batches. Normal HTTP redirects are unavailable. DNS-01 never opens a window.</p>{:else if http.mode==='off'}<p class="muted">Managed HTTP validation is off. Use DNS-01 or separately provision validation ingress.</p>{:else}<p class="muted">Normal HTTP redirects are enabled where configured; external ingress remains operator-managed.</p>{/if}
    {/if}
    <details open><summary>Listener profiles</summary>
      <h4>Legacy listener</h4>
      <NginxFields fields={sharedFields.slice(0,3)} bind:value={draft.shared} {disabled}/>
      <p class="muted">Used by: {used('listener','legacy').join(', ') || 'No saved references'}</p>
      {#each Object.entries(draft.listener_profiles) as [name,profile] (name)}
        <div class="editor-card">
          <h4>{name}</h4><label>Listener name<input value={name} required pattern="[A-Za-z0-9_]{1,32}" onchange={e=>rename('listener',name,e.currentTarget)}/></label><p class="muted">Used by: {used('listener',name).join(', ') || 'No saved references'}</p>
          {#each ['https','http'] as protocol}
            {@const p = protocol as 'http'|'https'}
            <h5>{protocol.toUpperCase()} endpoints</h5>
            {#each profile[p] as endpoint,i}
              <div class="editor-grid"><label>Listen address<input bind:value={endpoint.address} required/></label><label>Port<input type="number" min="1" max="65535" required value={endpoint.port} oninput={e=>endpoint.port=number(e.currentTarget.value)}/></label><button type="button" onclick={()=>endpoints(profile,p,profile[p].filter((_,j)=>j!==i))}>Remove endpoint</button></div>
            {/each}
            <button type="button" onclick={()=>endpoints(profile,p,[...profile[p],{address:'127.0.0.1',port:p==='https'?8444:8080}])}>Add {protocol.toUpperCase()} endpoint</button>
          {/each}
          <label><input type="checkbox" bind:checked={profile.http_redirect}/> HTTP redirect</label>
          <button type="button" onclick={()=>remove('listener',name)}>Remove listener</button>
        </div>
      {/each}
      <label>New listener name<input bind:value={listenerName}/></label><button type="button" onclick={()=>add('listener')}>Add listener</button>
    </details>
    <details open><summary>Client access rules</summary>
      <label>Legacy allowed IPs/networks (one per line)<textarea bind:value={allowed} rows="4"></textarea></label>
      <p class="muted">Used by: {used('access','legacy').join(', ') || 'No saved references'}</p>
      {#each Object.entries(draft.access_policies) as [name,policy] (name)}
        <div class="editor-card"><h4>{name}</h4><label>Access rule name<input value={name} required pattern="[A-Za-z0-9_]{1,32}" onchange={e=>rename('access',name,e.currentTarget)}/></label><p class="muted">Used by: {used('access',name).join(', ') || 'No saved references'}</p>
          <label>Allowed IPs/networks<textarea rows="4" value={policy.allow.join('\n')} onchange={e=>policy.allow=lines(e.currentTarget.value)}></textarea></label>
          <button type="button" onclick={()=>remove('access',name)}>Remove access rule</button>
        </div>
      {/each}
      <label>New access rule name<input bind:value={accessName}/></label><button type="button" onclick={()=>add('access')}>Add access rule</button>
    </details>
    <details><summary>Request limit policies</summary>
      <p class="muted">Built-in default: 30r/s, burst 60, immediate, 30 concurrent requests.</p>
      {#each Object.entries(draft.limit_policies) as [name,limit] (name)}
        <div class="editor-card"><h4>{name}</h4><p class="muted">Used by: {used('limit',name).join(', ') || 'No saved references'}</p><div class="editor-grid">
          <label>Rate<input bind:value={limit.rate} required placeholder="30r/s"/></label>
          <label>Burst<input type="number" min="0" max="100000" required value={limit.burst} oninput={e=>limit.burst=number(e.currentTarget.value)}/></label>
          <label>Concurrent connections<input type="number" min="1" max="65535" required value={limit.connections} oninput={e=>limit.connections=number(e.currentTarget.value)}/></label>
          <label><input type="checkbox" bind:checked={limit.nodelay}/> No delay</label></div>
          <button type="button" onclick={()=>remove('limit',name)}>Remove limit policy</button>
        </div>
      {/each}
      <label>New limit policy name<input bind:value={limitName}/></label><button type="button" onclick={()=>add('limit')}>Add limit policy</button>
    </details>
    <details><summary>Default site settings</summary>
      <p class="muted">Changing defaults affects all managed sites that inherit them.</p>
      <NginxPolicyEditor bind:value={draft.site_defaults} global={draft} defaults {disabled}/>
    </details>
    <details><summary>Advanced global settings</summary>
      <NginxFields fields={sharedFields.slice(3)} bind:value={draft.shared} {disabled}/>
      {#if draft.shared.nginx_daemon===false}<p class="form-error" role="alert">Warning: foreground Nginx prevents startup scripts from continuing. Disable background mode only for a foreground supervisor.</p>{/if}
      <label>Shared domain suffixes (one per line)<textarea bind:value={suffixes} rows="4"></textarea></label>
    </details>
    <p class="muted">Save validates the full configuration, regenerates managed sites, tests Nginx and reloads if running. Invalid changes are rolled back by the server.</p>
    {#if error}<p class="form-error" role="alert">{error}</p>{/if}
    <button type="submit">Save global settings</button>
  </fieldset><button type="button" onclick={onCancel}>Close editor</button>
</form>
<style>
  fieldset{border:0;padding:0;margin:0;min-width:0}
  .editor-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:12px;align-items:end}
  details{margin:12px 0;padding:10px;border:1px solid var(--border, #8884);border-radius:6px}
  .editor-card{border-top:1px solid var(--border,#8884);padding:12px 0}
  textarea{width:100%;font-family:monospace}
</style>

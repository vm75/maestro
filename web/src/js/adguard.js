    async function loadAdguardStatusOnly() { if (!state.adguard?.enabled) return; try { const st = await api('/adguard/status'); state.adguardStatus = st; renderAdguardStatus(st) } catch (e) { document.getElementById('adguard-status').innerHTML = status(true, 'Enabled', 'Disabled') } }
    async function loadAdguard() { try { const cfg = await api('/adguard/config'); state.adguard = cfg; const f = document.getElementById('adguard-form'); if (f) { f.elements.enabled.checked = !!cfg.enabled; f.elements.api_endpoint.value = cfg.api_endpoint || ''; f.elements.launch_url.value = cfg.launch_url || ''; f.elements.username.value = cfg.username || ''; f.elements.password.value = ''; f.elements.lan_ip.value = cfg.lan_ip || '' } if (cfg.enabled) { await loadAdguardStatusOnly() } else { document.getElementById('adguard-status').innerHTML = status(false, 'Enabled', 'Disabled'); renderAdguardStatus(null) } if (state.view === 'adguard') startAdguardPolling(); } catch (e) { document.getElementById('adguard-status').textContent = 'API Error'; toast(e.message, true) } }
    function renderAdguardStatus(st) {
      const statusEl = document.getElementById('adguard-status'),
        filtStatusEl = document.getElementById('filtering-status'),
        protStatusEl = document.getElementById('protection-status'),
        resumeFiltBtn = document.getElementById('resume-filtering-btn'),
        resumeProtBtn = document.getElementById('resume-protection-btn');

      if (!st || !st.enabled) {
        if (statusEl) statusEl.innerHTML = status(false, 'Enabled', 'Disabled');
        if (filtStatusEl) filtStatusEl.innerHTML = '<span class="badge gray">Disabled</span>';
        if (protStatusEl) protStatusEl.innerHTML = '<span class="badge gray">Disabled</span>';
        if (resumeFiltBtn) resumeFiltBtn.style.display = 'none';
        if (resumeProtBtn) resumeProtBtn.style.display = 'none';
        return;
      }
      let filtText = '';
      if (!st.filtering_enabled && st.filtering_remaining_seconds) {
        const m = Math.ceil(st.filtering_remaining_seconds / 60);
        filtText = `<span class="badge warning" style="background:#8a6d3b;color:#fff;padding:2px 6px;border-radius:4px">Paused (${m} min left)</span>`;
        if (resumeFiltBtn) resumeFiltBtn.style.display = '';
      } else if (!st.filtering_enabled) {
        filtText = `<span class="badge danger" style="background:#a94442;color:#fff;padding:2px 6px;border-radius:4px">Disabled</span>`;
        if (resumeFiltBtn) resumeFiltBtn.style.display = '';
      } else {
        filtText = `<span class="badge success" style="background:#3c763d;color:#fff;padding:2px 6px;border-radius:4px">Active</span>`;
        if (resumeFiltBtn) resumeFiltBtn.style.display = 'none';
      }
      if (filtStatusEl) filtStatusEl.innerHTML = filtText;
      
      let protText = '';
      if (!st.protection_enabled && st.protection_remaining_seconds) {
        const m = Math.ceil(st.protection_remaining_seconds / 60);
        protText = `<span class="badge warning" style="background:#8a6d3b;color:#fff;padding:2px 6px;border-radius:4px">Paused (${m} min left)</span>`;
        if (resumeProtBtn) resumeProtBtn.style.display = '';
      } else if (!st.protection_enabled) {
        protText = `<span class="badge danger" style="background:#a94442;color:#fff;padding:2px 6px;border-radius:4px">Disabled</span>`;
        if (resumeProtBtn) resumeProtBtn.style.display = '';
      } else {
        protText = `<span class="badge success" style="background:#3c763d;color:#fff;padding:2px 6px;border-radius:4px">Active</span>`;
        if (resumeProtBtn) resumeProtBtn.style.display = 'none';
      }
      if (protStatusEl) protStatusEl.innerHTML = protText;
      
      if (statusEl) {
        let summary = status(true, 'Enabled', 'Disabled');
        if (!st.filtering_enabled && st.filtering_remaining_seconds) {
          summary += ` &middot; Filtering Paused (${Math.ceil(st.filtering_remaining_seconds / 60)}m)`;
        } else if (!st.filtering_enabled) {
          summary += ` &middot; Filtering Disabled`;
        }
        if (!st.protection_enabled && st.protection_remaining_seconds) {
          summary += ` &middot; Protection Paused (${Math.ceil(st.protection_remaining_seconds / 60)}m)`;
        } else if (!st.protection_enabled) {
          summary += ` &middot; Protection Disabled`;
        }
        statusEl.innerHTML = summary;
      }
    }
    function renderAdguardRewrites() { const body = document.getElementById('adguard-rewrites-body'); body.innerHTML = state.adguardRewrites.length ? state.adguardRewrites.map((rewrite, index) => `<tr><td><input class="input" value="${esc(rewrite.domain)}" aria-label="Rewrite domain" oninput="adguardRewriteChanged(${index},'domain',this.value)"></td><td><input class="input" value="${esc(rewrite.answer)}" aria-label="Rewrite answer" oninput="adguardRewriteChanged(${index},'answer',this.value)"></td><td><button class="btn small danger" onclick="removeAdguardRewrite(${index})">Delete</button></td></tr>`).join('') : emptyRow(3, 'No custom DNS rewrites'); updateSortHeaders('rewrites') }
    function adguardRewriteChanged(index, key, value) { if (state.adguardRewrites[index]) state.adguardRewrites[index][key] = value }
    function addAdguardRewrite() { state.adguardRewrites.push({ domain: '', answer: '' }); renderAdguardRewrites(); const inputs = document.querySelectorAll('#adguard-rewrites-body input'); inputs[inputs.length - 2]?.focus() }
    function removeAdguardRewrite(index) { state.adguardRewrites.splice(index, 1); renderAdguardRewrites() }
    async function openAdguardRewrites() { if (!state.adguard?.enabled) return toast('Enable AdGuard integration first', true); const modal = document.getElementById('rewrites-modal'); try { state.adguardRewrites = await api('/adguard/rewrites'); renderAdguardRewrites(); modal.showModal() } catch (e) { toast(e.message, true) } }
    function closeAdguardRewrites() { document.getElementById('rewrites-modal').close() }
    async function saveAdguardRewrites() { try { state.adguardRewrites = await api('/adguard/rewrites', { method: 'PUT', body: JSON.stringify(state.adguardRewrites) }); closeAdguardRewrites(); toast('DNS rewrites saved') } catch (e) { toast(e.message, true) } }
    function renderAdguardHosts() { const body = document.getElementById('adguard-hosts-body'); body.innerHTML = state.adguardHosts.length ? state.adguardHosts.map((entry, index) => `<tr><td><input class="input" value="${esc(entry.ip)}" aria-label="Host IP address" oninput="adguardHostChanged(${index},'ip',this.value)"></td><td><input class="input" value="${esc(entry.hostnames.join(' '))}" aria-label="Hostnames" oninput="adguardHostChanged(${index},'hostnames',this.value)"></td><td><button class="btn small danger" onclick="removeAdguardHost(${index})">Delete</button></td></tr>`).join('') : emptyRow(3, 'No host mappings'); updateSortHeaders('hosts') }
    function adguardHostChanged(index, key, value) { if (state.adguardHosts[index]) state.adguardHosts[index][key] = key === 'hostnames' ? value.trim().split(/\s+/).filter(Boolean) : value }
    function addAdguardHost() { state.adguardHosts.push({ ip: '', hostnames: [] }); renderAdguardHosts(); const inputs = document.querySelectorAll('#adguard-hosts-body input'); inputs[inputs.length - 2]?.focus() }
    function removeAdguardHost(index) { state.adguardHosts.splice(index, 1); renderAdguardHosts() }
    async function openAdguardHosts() { const modal = document.getElementById('hosts-modal'); try { state.adguardHosts = await api('/adguard/hosts'); renderAdguardHosts(); modal.showModal() } catch (e) { toast(e.message, true) } }
    function closeAdguardHosts() { document.getElementById('hosts-modal').close() }
    async function saveAdguardHosts() { try { state.adguardHosts = await api('/adguard/hosts', { method: 'PUT', body: JSON.stringify(state.adguardHosts) }); closeAdguardHosts(); toast('hosts.add saved; dnsmasq restarted') } catch (e) { toast(e.message, true) } }
    function renderDnsmasqHosts() { const body = document.getElementById('dnsmasq-hosts-body'); body.innerHTML = state.dnsmasqHosts.length ? state.dnsmasqHosts.map((entry, index) => `<tr><td><input class="input" value="${esc(entry.mac)}" oninput="dnsmasqHostChanged(${index},'mac',this.value)"></td><td><input class="input" value="${esc(entry.hostname)}" oninput="dnsmasqHostChanged(${index},'hostname',this.value)"></td><td><input class="input" value="${esc(entry.ip)}" oninput="dnsmasqHostChanged(${index},'ip',this.value)"></td><td><button class="btn small danger" onclick="removeDnsmasqHost(${index})">Delete</button></td></tr>`).join('') : emptyRow(4, 'No DHCP reservations'); updateSortHeaders('dnsmasq') }
    function dnsmasqHostChanged(index, key, value) { if (state.dnsmasqHosts[index]) state.dnsmasqHosts[index][key] = value }
    function addDnsmasqHost() { state.dnsmasqHosts.push({ mac: '', hostname: '', ip: '' }); renderDnsmasqHosts(); document.querySelector('#dnsmasq-hosts-body input')?.focus() }
    function removeDnsmasqHost(index) { state.dnsmasqHosts.splice(index, 1); renderDnsmasqHosts() }
    async function openDnsmasqHosts() { const modal = document.getElementById('dnsmasq-hosts-modal'); try { state.dnsmasqHosts = await api('/adguard/dnsmasq-hosts'); renderDnsmasqHosts(); modal.showModal() } catch (e) { toast(e.message, true) } }
    function closeDnsmasqHosts() { document.getElementById('dnsmasq-hosts-modal').close() }


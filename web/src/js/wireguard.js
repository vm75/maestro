    async function loadWireguardStatus() { try { const st = await api('/wireguard/status'); const badge = document.getElementById('wireguard-status-badge'); if (badge) { badge.innerHTML = status(st.is_running); } } catch(e) {} }
    async function loadWireguard() { try { const cfg = await api('/wireguard/config'); state.wireguard = cfg; const f = document.getElementById('wireguard-form'); if (f) { f.elements.private_key.value = cfg.interface.private_key || ''; f.elements.address.value = cfg.interface.address || ''; f.elements.listen_port.value = cfg.interface.listen_port || ''; f.elements.post_up.value = cfg.interface.post_up || ''; f.elements.post_down.value = cfg.interface.post_down || ''; } renderWireguardPeers(); await loadWireguardStatus(); } catch (e) { toast(e.message, true); } }
    async function saveWireguardSettings(e) { e.preventDefault(); const f = e.target; const cfg = state.wireguard || { interface: {}, peers: [] }; cfg.interface = { private_key: f.elements.private_key.value, address: f.elements.address.value, listen_port: f.elements.listen_port.value ? parseInt(f.elements.listen_port.value, 10) : null, post_up: f.elements.post_up.value || null, post_down: f.elements.post_down.value || null }; try { state.wireguard = await api('/wireguard/config', { method: 'PUT', body: JSON.stringify(cfg) }); toast('WireGuard settings saved'); renderWireguardPeers(); } catch (err) { toast(err.message, true); } }
    async function toggleWireguard(enable) { try { const res = await api('/wireguard/toggle', { method: 'POST', body: JSON.stringify({ enable }) }); toast(res.message); loadWireguardStatus(); } catch(e) { toast(e.message, true); } }
    function renderWireguardPeers() { const body = document.getElementById('wireguard-peers-body'); const peers = state.wireguard?.peers || []; body.innerHTML = peers.length ? peers.map((p, i) => `<tr><td><input class="input" value="${esc(p.public_key)}" placeholder="Public Key" oninput="wgPeerChange(${i},'public_key',this.value)"></td><td><input class="input" value="${esc(p.allowed_ips)}" placeholder="Allowed IPs" oninput="wgPeerChange(${i},'allowed_ips',this.value)"></td><td><input class="input" value="${esc(p.endpoint||'')}" placeholder="Endpoint" oninput="wgPeerChange(${i},'endpoint',this.value)"></td><td><button class="btn small danger" onclick="removeWireguardPeer(${i})">Delete</button></td></tr>`).join('') : emptyRow(4, 'No peers configured'); }
    function wgPeerChange(i, k, v) { if(state.wireguard.peers[i]) state.wireguard.peers[i][k] = v; }
    function addWireguardPeer() { if(!state.wireguard) state.wireguard = { interface: {}, peers: [] }; state.wireguard.peers.push({ public_key: '', allowed_ips: '', endpoint: null, preshared_key: null, persistent_keepalive: null }); renderWireguardPeers(); const inputs = document.querySelectorAll('#wireguard-peers-body input'); inputs[inputs.length - 3]?.focus(); }
    function removeWireguardPeer(i) { state.wireguard.peers.splice(i, 1); renderWireguardPeers(); }
    async function saveDnsmasqHosts() { try { state.dnsmasqHosts = await api('/adguard/dnsmasq-hosts', { method: 'PUT', body: JSON.stringify(state.dnsmasqHosts) }); closeDnsmasqHosts(); toast('dnsmasq.conf.add saved; dnsmasq restarted') } catch (e) { toast(e.message, true) } }
    function launchAdGuard() { if (state.adguard?.launch_url) window.open(state.adguard.launch_url, '_blank'); else if (state.adguard?.api_endpoint) window.open(state.adguard.api_endpoint, '_blank'); else toast('API endpoint not configured', true) }
    async function toggleAdguardFiltering(enabled, duration_minutes) { try { const p = { enabled, duration_minutes: duration_minutes || null }; await api('/adguard/filtering', { method: 'POST', body: JSON.stringify(p) }); toast(`AdGuard filtering ${enabled ? 'enabled' : `paused for ${duration_minutes} min`}`); loadAdguardStatusOnly() } catch (e) { toast(e.message, true) } }
    async function toggleAdguardProtection(enabled, duration_minutes) { try { const p = { enabled, duration_minutes: duration_minutes || null }; await api('/adguard/protection', { method: 'POST', body: JSON.stringify(p) }); toast(`AdGuard protection ${enabled ? 'enabled' : `paused for ${duration_minutes} min`}`); loadAdguardStatusOnly() } catch (e) { toast(e.message, true) } }
    async function saveAdGuardSettings(e) { if (e) e.preventDefault(); try { const f = document.getElementById('adguard-form'), v = formObject(f), payload = { enabled: f.elements.enabled.checked, api_endpoint: v.api_endpoint, launch_url: v.launch_url || '', username: v.username, password: v.password || undefined, lan_ip: v.lan_ip }; await api('/adguard/config', { method: 'PUT', body: JSON.stringify(payload) }); toast('AdGuard settings saved'); loadAdguard() } catch (err) { toast(err.message, true) } }

    function applyCertificateLockUi() {
      const lock = state.dehydratedLock || {};
      const locked = !!lock.locked;
      const activity = lock.status || (locked ? 'locked' : 'idle');
      const operation = lock.operation;
      const running = activity === 'running';
      const certificate = operation?.certificate ? ` for ${operation.certificate}` : '';
      const action = operation?.action === 'issue' ? 'issuance' : 'renewal';
      const notice = document.getElementById('certificate-lock-notice');
      const message = document.getElementById('certificate-lock-message');
      const path = document.getElementById('certificate-lock-path');
      const clear = document.getElementById('certificate-lock-clear');

      if (notice) {
        notice.hidden = activity === 'idle';
        notice.className = 'notice' + (['failed', 'locked'].includes(activity) ? ' warn' : '');
      }
      if (message) {
        message.textContent = activity === 'running'
          ? `Dehydrated is running${certificate}. Certificate issue and renewal are temporarily unavailable.`
          : activity === 'failed'
            ? `Certificate ${action} failed${certificate}.${operation?.error ? ` ${operation.error}` : ''}${locked ? ' A lock file remains.' : ''}`
            : activity === 'completed'
              ? `Certificate ${action} completed${certificate}.`
              : activity === 'locked'
                ? 'A dehydrated lock file remains, but no active dehydrated process was detected. Clear it only after verifying that no certificate hook is running.'
                : '';
      }
      if (path) {
        path.hidden = !locked;
        path.textContent = locked ? lock.path : '';
      }
      if (clear) clear.hidden = !(locked && !running);
      document.querySelectorAll('#certs-body .row-actions button').forEach(button => {
        const renewalAction = ['Renew', 'Issue'].includes(button.textContent.trim());
        button.disabled = renewalAction && (locked || running);
        button.title = renewalAction && (locked || running) ? 'A certificate operation or lock is active' : '';
      });
    }
    async function loadCertificateHttpAccess() {
      try {
        const value=await api('/certificates/http-access'), output=document.getElementById('certificate-http-status');
        const labels={closed:'HTTP closed',active:'Certificate validation in progress',always_on:'Always-on HTTP configured',cleanup_failed:'Cleanup failed / exposure unknown',expired_cleanup_pending:'Window expired; local cleanup pending',unknown:'HTTP state not yet verified'};
        output.textContent=`${labels[value.state] || value.state}${value.expires_at ? ' · deadline '+prettyDate(value.expires_at) : ''} · WAN: ${({managed_ipv4_kernel_expiry_ipv6_blocked:'Router-managed IPv4 (IPv6 blocked)',external:'Externally managed',unsupported:'Automatic control unavailable',unconfigured:'Not configured'})[value.wan_control] || value.wan_control}${value.error ? ' · '+value.error : ''}`;
        output.className=['cleanup_failed','expired_cleanup_pending','unknown'].includes(value.state) ? 'route-policy-error' : 'muted';
      } catch(error) { document.getElementById('certificate-http-status').textContent=`HTTP status unavailable: ${error.message}`; }
    }
    async function closeCertificateHttpAccess() {
      try { await api('/certificates/http-access',{method:'DELETE'}); await loadCertificateHttpAccess(); toast('HTTP cleanup completed'); }
      catch(error) { toast(error.message,true); await loadCertificateHttpAccess(); }
    }
    async function loadCertificateLock() {
      state.dehydratedLock = await api('/certificates/dehydrated/lock');
      applyCertificateLockUi();
      await loadCertificateHttpAccess();
    }
    async function loadCertificatesWithLock() {
      await loadCertificates();
      await loadCertificateLock();
      if (state.view === 'certificates') startCertificatePolling();
    }
    async function clearDehydratedLock() {
      if (!confirm('Clear the dehydrated lock file? Only do this after confirming no dehydrated process is running.')) return;
      try {
        state.dehydratedLock = await api('/certificates/dehydrated/lock', { method: 'DELETE' });
        applyCertificateLockUi();
        toast('Dehydrated lock cleared');
        await loadCertificatesWithLock();
      } catch (e) { toast(e.message, true) }
    }
    new MutationObserver(applyCertificateLockUi).observe(document.getElementById('certs-body'), { childList: true });


    async function loadDashboard() {
      const grid = document.getElementById('dashboard-grid');
      const widgets = [...new Set(rhModuleCatalog
        .filter(m => m.availability !== 'unsupported' && m.availability !== 'disabled')
        .flatMap(m => (m.ui || []).flatMap(section => section.dashboard_widgets || [])))];
      const owners = { services: 'services', nginx: 'nginx', certificates: 'certificates', active_bans: 'firewall' };
      const loaders = {
        services: async () => { const items = await api('/services'); return ['Services',
          items.filter(s => s.running).length + '/' + items.length, 'running']; },
        nginx: async () => { const [server, items] = await Promise.all([api('/nginx/status'), api('/nginx/objects')]);
          return ['Nginx objects', items.length, server.running ? 'nginx online' : 'nginx offline']; },
        certificates: async () => { const items = await api('/certificates');
          const due = items.filter(item => item.renewal_due).length;
          return ['Certificates', items.length, due ? due + ' need attention' : 'renewals healthy']; },
        active_bans: async () => { const status = await api('/firewall?include_bans=false');
          const summary = status.snapshot || {};
          return ['Active bans', summary.active_ban_count ?? 0,
            (summary.banned_subnets || []).length + ' subnet · ' + (summary.banned_ips || []).length + ' IP']; }
      };
      grid.innerHTML = widgets.map(id => '<div class="card metric" data-dashboard-widget="' +
        esc(id) + '" data-widget-state="loading"><div class="label">' + esc(id) +
        '</div><div class="value">Loading…</div></div>').join('');
      await Promise.all(widgets.map(async id => {
        const card = [...grid.querySelectorAll('[data-dashboard-widget]')].find(el => el.dataset.dashboardWidget === id);
        if (!card) return;
        try {
          if (!loaders[id]) throw new Error('Widget not available');
          const [label, value, detail] = await loaders[id]();
          card.innerHTML = '<div class="label">' + esc(label) + '</div><div class="value">' +
            esc(value) + '</div><div class="sub">' + esc(detail) + '</div>';
          card.dataset.widgetState = 'ready';
          if (owners[id]) {
            card.setAttribute('role', 'button');
            card.setAttribute('tabindex', '0');
            card.onclick = () => rhSelectView(owners[id]);
            card.onkeydown = e => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); rhSelectView(owners[id]); } };
          }
        } catch (error) {
          card.dataset.widgetState = 'unavailable';
          card.innerHTML = '<div class="label">' + esc(id) +
            '</div><div class="value">Unavailable</div><div class="sub">' + esc(error.message) + '</div>';
        }
      }));
      try {
        const info = await api('/runtime');
        const versionEl = document.getElementById('rh-version');
        if (versionEl) versionEl.textContent = 'v' + info.version;
        const modeEl = document.getElementById('rh-mode');
        if (modeEl) {
          modeEl.textContent = info.test_mode ? 'x64 test mode' :
            info.platform === 'linux' ? 'linux' : 'asuswrt';
          modeEl.style.display = 'inline-flex';
        }
      } catch (_) { /* Widget failures do not block the dashboard shell. */ }
    }


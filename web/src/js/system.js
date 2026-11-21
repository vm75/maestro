    let lastUsage = null;
    let loadHistory1 = [];
    let loadHistory5 = [];
    let loadHistory15 = [];
    let netHistory = [];
    const MAX_HISTORY = 60;
    
    function formatBytes(bytes) {
      if (bytes < 1024) return bytes + ' B';
      const k = bytes / 1024;
      if (k < 1024) return k.toFixed(2) + ' KB';
      const m = k / 1024;
      if (m < 1024) return m.toFixed(2) + ' MB';
      return (m / 1024).toFixed(2) + ' GB';
    }
    
    function formatUptime(secs) {
      const d = Math.floor(secs / 86400);
      const h = Math.floor((secs % 86400) / 3600);
      const m = Math.floor((secs % 3600) / 60);
      const s = Math.floor(secs % 60);
      if (d > 0) return `${d} days, ${h}h ${m}m ${s}s`;
      if (h > 0) return `${h}h ${m}m ${s}s`;
      return `${m}m ${s}s`;
    }

    async function pollSystemUsage() {
      if (state.view !== 'dashboard') return;
      try {
        const usage = await api('/system/usage');
        document.getElementById('system-usage-grid').style.display = 'grid';
        
        // RAM
        const mem = usage.memory;
        const ramUsed = mem.total_kb - (mem.available_kb || mem.free_kb);
        const ramPct = mem.total_kb > 0 ? (ramUsed / mem.total_kb * 100).toFixed(0) : 0;
        document.getElementById('txt-ram-used').textContent = (ramUsed / 1024).toFixed(0) + ' MB';
        document.getElementById('txt-ram-free').textContent = ((mem.available_kb || mem.free_kb) / 1024).toFixed(0) + ' MB';
        document.getElementById('txt-ram-total').textContent = (mem.total_kb / 1024).toFixed(0) + ' MB';
        document.getElementById('bar-ram').style.width = ramPct + '%';
        document.getElementById('txt-ram-pct').textContent = ramPct + '%';
        
        // Swap
        const swapUsed = mem.swap_total_kb - mem.swap_free_kb;
        const swapPct = mem.swap_total_kb > 0 ? (swapUsed / mem.swap_total_kb * 100).toFixed(0) : 0;
        document.getElementById('txt-swap-used').textContent = (swapUsed / 1024).toFixed(0) + ' MB';
        document.getElementById('txt-swap-free').textContent = (mem.swap_free_kb / 1024).toFixed(0) + ' MB';
        document.getElementById('txt-swap-total').textContent = (mem.swap_total_kb / 1024).toFixed(0) + ' MB';
        document.getElementById('bar-swap').style.width = swapPct + '%';
        document.getElementById('txt-swap-pct').textContent = swapPct + '%';
        
        // Uptime & Load Avg
        document.getElementById('txt-uptime').textContent = formatUptime(usage.uptime_seconds);
        if (usage.load_average) {
          document.getElementById('txt-loadavg').textContent = `Load Average: ${usage.load_average.map(n => n.toFixed(2)).join(', ')}`;
        }

        if (lastUsage) {
          // Network
          const dlBytes = usage.network.rx_bytes - lastUsage.network.rx_bytes;
          const ulBytes = usage.network.tx_bytes - lastUsage.network.tx_bytes;
          const dlSpeed = dlBytes > 0 ? dlBytes : 0;
          const ulSpeed = ulBytes > 0 ? ulBytes : 0;
          document.getElementById('txt-dl').textContent = formatBytes(dlSpeed) + '/s';
          document.getElementById('txt-ul').textContent = formatBytes(ulSpeed) + '/s';
          
          // Network Bars (relative to 10MB/s for visual scaling, cap at 100%)
          const maxSpeed = 10 * 1024 * 1024;
          document.getElementById('bar-dl').style.width = Math.min(100, (dlSpeed / maxSpeed) * 100) + '%';
          document.getElementById('bar-ul').style.width = Math.min(100, (ulSpeed / maxSpeed) * 100) + '%';
          
          // CPU Cores
          let overallCpuPct = 0;
          let coreHtml = '';
          const currentCpus = usage.cpu;
          const prevCpus = lastUsage.cpu;
          let maxCorePct = 0;
          
          for (let i = 0; i < currentCpus.length; i++) {
            const curr = currentCpus[i];
            const prev = prevCpus.find(p => p.name === curr.name);
            if (prev) {
              const currTotal = curr.user + curr.nice + curr.system + curr.idle + curr.iowait + curr.irq + curr.softirq + curr.steal;
              const prevTotal = prev.user + prev.nice + prev.system + prev.idle + prev.iowait + prev.irq + prev.softirq + prev.steal;
              const totalDiff = currTotal - prevTotal;
              const currIdle = curr.idle + curr.iowait;
              const prevIdle = prev.idle + prev.iowait;
              const idleDiff = currIdle - prevIdle;
              
              const pct = totalDiff > 0 ? ((totalDiff - idleDiff) / totalDiff) * 100 : 0;
              
              if (curr.name === 'cpu') {
                overallCpuPct = pct;
              } else {
                maxCorePct = Math.max(maxCorePct, pct);
                const colors = ['#ffcb66', '#31d7dc', '#ff6b76', '#4cdb98'];
                const color = colors[(i - 1) % colors.length];
                coreHtml += `<div style="display:flex;align-items:center;">
                  <div style="flex:1;background:#101112;height:12px;border-radius:6px;overflow:hidden;position:relative;">
                    <div style="width:${pct}%;height:100%;background:${color};border-radius:6px;"></div>
                  </div>
                  <div style="width:120px;text-align:right;font-size:12px;display:flex;justify-content:space-between;padding-left:12px;"><span style="color:#b0bec5">${curr.name.toUpperCase()}</span><span>${pct.toFixed(0)}%</span></div>
                </div>`;
              }
            }
          }
          document.getElementById('cpu-cores').innerHTML = coreHtml;
          
          // History
          const load1 = usage.load_average ? usage.load_average[0] : 0;
          const load5 = usage.load_average ? usage.load_average[1] : 0;
          const load15 = usage.load_average ? usage.load_average[2] : 0;
          
          loadHistory1.push(load1);
          loadHistory5.push(load5);
          loadHistory15.push(load15);
          if (loadHistory1.length > MAX_HISTORY) loadHistory1.shift();
          if (loadHistory5.length > MAX_HISTORY) loadHistory5.shift();
          if (loadHistory15.length > MAX_HISTORY) loadHistory15.shift();
          
          // Draw Canvas
          const canvas = document.getElementById('cpu-canvas');
          const ctx = canvas.getContext('2d');
          
          // Handle High DPI displays
          const rect = canvas.getBoundingClientRect();
          const dpr = window.devicePixelRatio || 1;
          canvas.width = rect.width * dpr;
          canvas.height = rect.height * dpr;
          ctx.scale(dpr, dpr);
          
          const cssWidth = rect.width;
          const cssHeight = rect.height;

          // Grid
          ctx.strokeStyle = '#22323a';
          ctx.lineWidth = 1;
          for(let i=1; i<4; i++) {
            ctx.beginPath();
            ctx.moveTo(0, cssHeight * i / 4);
            ctx.lineTo(cssWidth, cssHeight * i / 4);
            ctx.stroke();
          }
          for(let i=1; i<10; i++) {
            ctx.beginPath();
            ctx.moveTo(cssWidth * i / 10, 0);
            ctx.lineTo(cssWidth * i / 10, cssHeight);
            ctx.stroke();
          }
          
          // Scale max to num cores or highest load
          const numCores = Math.max(1, currentCpus.length - 1);
          const maxLoad = Math.max(numCores, ...loadHistory1, ...loadHistory5, ...loadHistory15);
          
          // Draw Line function
          const drawLine = (history, color, width) => {
            if (history.length < 2) return;
            ctx.beginPath();
            ctx.strokeStyle = color;
            ctx.lineWidth = width;
            const step = cssWidth / (MAX_HISTORY - 1);
            for (let i = 0; i < history.length; i++) {
              const x = i * step;
              const y = cssHeight - (history[i] / maxLoad * cssHeight);
              if (i === 0) ctx.moveTo(x, y);
              else ctx.lineTo(x, y);
            }
            ctx.stroke();
          };

          // Draw load lines (15m, 5m, 1m)
          drawLine(loadHistory15, 'rgba(255, 203, 102, 0.4)', 2);
          drawLine(loadHistory5, 'rgba(255, 107, 118, 0.6)', 2);
          drawLine(loadHistory1, '#31d7dc', 2);
          
          // Fill under 1m
          if (loadHistory1.length > 1) {
            const step = cssWidth / (MAX_HISTORY - 1);
            ctx.lineTo((loadHistory1.length - 1) * step, cssHeight);
            ctx.lineTo(0, cssHeight);
            ctx.fillStyle = 'rgba(49, 215, 220, 0.1)';
            ctx.fill();
          }
          
          // Text overlays
          ctx.fillStyle = '#b0bec5';
          ctx.font = '10px Arial';
          ctx.fillText(maxLoad.toFixed(1), 2, 10);
          ctx.fillText((maxLoad / 2).toFixed(1), 2, cssHeight / 2 + 4);
          ctx.fillText('0.0', 2, cssHeight - 2);
          
          // Legend
          const legend = [
            { label: '1m', color: '#31d7dc' },
            { label: '5m', color: '#ff6b76' },
            { label: '15m', color: '#ffcb66' }
          ];
          let legendX = cssWidth - 10;
          ctx.textAlign = 'right';
          for (let i = legend.length - 1; i >= 0; i--) {
            ctx.fillStyle = '#b0bec5';
            ctx.fillText(legend[i].label, legendX, 10);
            legendX -= ctx.measureText(legend[i].label).width + 4;
            
            ctx.fillStyle = legend[i].color;
            ctx.fillRect(legendX - 8, 4, 6, 6);
            legendX -= 14;
          }
          ctx.textAlign = 'left'; // reset
        }
        
        lastUsage = usage;
      } catch (e) {
        console.error('Failed to poll system usage:', e);
      }
    }
    
    setInterval(pollSystemUsage, 1000);

    refreshCurrent();
    rhLoadModules();

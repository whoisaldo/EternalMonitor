/* EternalMonitor — Site Scripts */

(function () {
  'use strict';

  /* --- macOS Notice --- */
  // iPadOS reports platform 'MacIntel' too, so require a non-touch device.
  var isMac = /Mac/.test(navigator.platform) && navigator.maxTouchPoints <= 1;
  if (isMac && sessionStorage.getItem('mac-notice-dismissed') !== '1') {
    var notice = document.createElement('div');
    notice.className = 'mac-notice';
    notice.innerHTML =
      '<p><strong>Looks like you\'re on a Mac.</strong>' +
      'EternalMonitor is Windows-only for now. The good news: macOS already does this ' +
      'for free with <a href="https://support.apple.com/en-us/102386" target="_blank" rel="noopener">Sidecar</a>, ' +
      'which turns your iPad into a second display natively and works well.</p>' +
      '<button type="button" class="mac-notice-close" aria-label="Dismiss">&times;</button>';
    notice.querySelector('.mac-notice-close').addEventListener('click', function () {
      sessionStorage.setItem('mac-notice-dismissed', '1');
      notice.remove();
    });
    document.body.appendChild(notice);
  }

  /* --- Scroll Reveal (IntersectionObserver) --- */
  var reveals = document.querySelectorAll('.reveal');
  if (reveals.length && 'IntersectionObserver' in window) {
    var observer = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          entry.target.classList.add('visible');
          observer.unobserve(entry.target);
        }
      });
    }, { threshold: 0.15, rootMargin: '0px 0px -40px 0px' });

    reveals.forEach(function (el) { observer.observe(el); });
  } else {
    // Fallback: show everything immediately
    reveals.forEach(function (el) { el.classList.add('visible'); });
  }

  /* --- Smooth Scroll for Anchor Links --- */
  document.querySelectorAll('a[href^="#"]').forEach(function (link) {
    link.addEventListener('click', function (e) {
      var target = document.querySelector(this.getAttribute('href'));
      if (target) {
        e.preventDefault();
        target.scrollIntoView({ behavior: 'smooth' });
      }
    });
  });

  /* --- GitHub Releases API Fetch --- */
  var downloadBtn = document.getElementById('download-btn');
  var versionEl = document.getElementById('release-version');
  var metaEl = document.getElementById('release-meta');
  var sha256El = document.getElementById('sha256-value');

  if (!downloadBtn) return; // Not on download page

  function formatBytes(bytes) {
    if (bytes < 1024) return bytes + ' B';
    if (bytes < 1048576) return (bytes / 1024).toFixed(1) + ' KB';
    return (bytes / 1048576).toFixed(1) + ' MB';
  }

  fetch('https://api.github.com/repos/whoisaldo/EternalMonitor/releases/latest', {
    headers: { 'Accept': 'application/vnd.github.v3+json' }
  })
    .then(function (res) {
      if (!res.ok) throw new Error('HTTP ' + res.status);
      return res.json();
    })
    .then(function (release) {
      var asset = release.assets.find(function (a) {
        return a.name.endsWith('.zip') || a.name.endsWith('.exe') || a.name.endsWith('.msi');
      });

      if (!asset) throw new Error('No Windows asset found');

      downloadBtn.href = asset.browser_download_url;
      downloadBtn.textContent = 'Download ' + asset.name;
      if (versionEl) versionEl.textContent = release.tag_name;
      if (metaEl) metaEl.textContent = asset.name + ' \u00B7 ' + formatBytes(asset.size);

      // Try to extract SHA256 from release body
      if (sha256El && release.body) {
        var match = release.body.match(/[a-fA-F0-9]{64}/);
        if (match) sha256El.textContent = match[0];
      }
    })
    .catch(function () {
      downloadBtn.href = 'https://github.com/whoisaldo/EternalMonitor/releases';
      downloadBtn.textContent = 'View Releases on GitHub';
      if (metaEl) metaEl.textContent = 'Visit GitHub for the latest release';
    });

  /* --- Preview build ---
     /releases/latest above deliberately skips pre-releases, so testers would
     never see a build that has not shipped yet. Look for the newest one and
     reveal the preview card only if it exists; once a stable release
     supersedes it, the card disappears on its own. */
  var previewSection = document.getElementById('preview-section');
  if (!previewSection) return;

  fetch('https://api.github.com/repos/whoisaldo/EternalMonitor/releases?per_page=10', {
    headers: { 'Accept': 'application/vnd.github.v3+json' }
  })
    .then(function (res) {
      if (!res.ok) throw new Error('HTTP ' + res.status);
      return res.json();
    })
    .then(function (releases) {
      var preview = releases.find(function (r) {
        return r.prerelease && !r.draft;
      });
      if (!preview) return; // nothing in testing right now

      var asset = preview.assets.find(function (a) {
        return a.name.endsWith('.exe') || a.name.endsWith('.zip') || a.name.endsWith('.msi');
      });
      if (!asset) return;

      var btn = document.getElementById('preview-btn');
      var version = document.getElementById('preview-version');
      var meta = document.getElementById('preview-meta');
      var sha = document.getElementById('preview-sha256');

      btn.href = asset.browser_download_url;
      btn.textContent = 'Download ' + asset.name;
      if (version) version.textContent = preview.tag_name;
      if (meta) meta.textContent = asset.name + ' \u00B7 ' + formatBytes(asset.size);
      if (sha && preview.body) {
        var match = preview.body.match(/[a-fA-F0-9]{64}/);
        if (match) sha.textContent = match[0];
      }
      previewSection.hidden = false;
    })
    .catch(function () {
      /* No preview, or the API is unreachable: leave the card hidden. */
    });
})();

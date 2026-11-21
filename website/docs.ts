const menuToggle = document.querySelector<HTMLButtonElement>('#docs-menu-toggle');

if (menuToggle) {
  const topicLinks = document.querySelectorAll<HTMLAnchorElement>('#docs-menu a');
  const updateTopic = (moveFocus = false) => {
    const target =
      document.getElementById(location.hash.slice(1)) ??
      document.querySelector('.docs-content :target');
    const topic =
      target?.closest<HTMLElement>('.docs-topic') ?? document.getElementById('quickstart');

    for (const link of topicLinks) {
      if (link.hash === `#${topic?.id}`) link.setAttribute('aria-current', 'location');
      else link.removeAttribute('aria-current');
    }
    menuToggle.setAttribute('aria-expanded', 'false');
    if (moveFocus) {
      const heading = topic?.querySelector('h1');
      const focusTarget =
        target?.tabIndex === -1 && target.hasAttribute('tabindex') ? target : heading;
      focusTarget?.focus({ preventScroll: true });
    }
  };

  menuToggle.hidden = false;
  menuToggle.addEventListener('click', () => {
    const isOpen = menuToggle.getAttribute('aria-expanded') === 'true';
    menuToggle.setAttribute('aria-expanded', String(!isOpen));
  });
  for (const link of topicLinks) {
    link.addEventListener('click', (event) => {
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      if (link.hash === location.hash) updateTopic(true);
    });
  }
  window.addEventListener('hashchange', () => updateTopic(true));
  window.addEventListener('pageshow', () => updateTopic());
  updateTopic();
}

const docsCopyStatus = document.querySelector('#docs-copy-status');

if (docsCopyStatus && navigator.clipboard) {
  for (const button of document.querySelectorAll<HTMLButtonElement>('[data-copy-code]')) {
    const code = document.getElementById(button.dataset.copyCode ?? '');
    if (!code) continue;
    let reset: number | undefined;
    button.hidden = false;
    button.addEventListener('click', async () => {
      window.clearTimeout(reset);
      try {
        await navigator.clipboard.writeText(code.textContent ?? '');
        docsCopyStatus.textContent = 'code copied.';
        button.textContent = 'copied';
        reset = window.setTimeout(() => {
          button.textContent = 'copy';
        }, 1600);
      } catch {
        getSelection()?.selectAllChildren(code);
        button.textContent = 'copy';
        docsCopyStatus.textContent = 'copy the selected code.';
      }
    });
  }
}

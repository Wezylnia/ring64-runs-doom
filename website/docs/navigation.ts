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
  const topicNav = menuToggle.closest('nav');
  topicNav?.addEventListener('focusout', (event) => {
    if (event.relatedTarget instanceof Node && !topicNav.contains(event.relatedTarget)) {
      menuToggle.setAttribute('aria-expanded', 'false');
    }
  });
  topicNav?.addEventListener('keydown', (event) => {
    if (
      event.key !== 'Escape' ||
      menuToggle.getAttribute('aria-expanded') !== 'true' ||
      !menuToggle.offsetHeight
    ) {
      return;
    }
    event.preventDefault();
    menuToggle.setAttribute('aria-expanded', 'false');
    menuToggle.focus();
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

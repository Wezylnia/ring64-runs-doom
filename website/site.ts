const command = document.querySelector('#install-command');
const copy = document.querySelector<HTMLButtonElement>('#copy-install');
const copyStatus = document.querySelector('#copy-status');

if (command && copy && copyStatus && navigator.clipboard) {
  let reset: number | undefined;
  copy.hidden = false;
  copy.addEventListener('click', async () => {
    window.clearTimeout(reset);
    try {
      await navigator.clipboard.writeText(command.textContent ?? '');
      copy.dataset.copied = 'true';
      copyStatus.textContent = 'install command copied.';
    } catch {
      getSelection()?.selectAllChildren(command);
      delete copy.dataset.copied;
      copyStatus.textContent = 'copy the selected install command.';
    }
    reset = window.setTimeout(() => {
      delete copy.dataset.copied;
      copyStatus.textContent = '';
    }, 1800);
  });
}

function isStarCount(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function loadGithubStars() {
  const github = document.querySelector('#github-link');
  const stars = document.querySelector<HTMLElement>('#github-stars');
  const value = document.querySelector('#github-star-value');
  if (!github || !stars || !value) return;

  const cacheKey = 'ring64:github-stars';
  const render = (count: number) => {
    const formatted = new Intl.NumberFormat().format(count);
    value.textContent = formatted;
    stars.hidden = false;
    github.setAttribute('aria-label', `github, ${formatted} ${count === 1 ? 'star' : 'stars'}`);
  };

  try {
    const stored = sessionStorage.getItem(cacheKey);
    const count = stored ? Number(stored) : undefined;
    if (isStarCount(count)) {
      render(count);
      return;
    }
  } catch {
    // Fetch normally if session storage is unavailable.
  }

  const canvas = document.querySelector<HTMLCanvasElement>('#ink');
  if (canvas && !canvas.dataset.frame) {
    // Keep the metadata request out of the first frame's loading path.
    const observer = new MutationObserver(load);
    const timeout = window.setTimeout(load, 3000);
    observer.observe(canvas, { attributes: true, attributeFilter: ['data-frame'] });

    function load() {
      observer.disconnect();
      window.clearTimeout(timeout);
      void fetchCount();
    }
  } else {
    void fetchCount();
  }

  async function fetchCount() {
    try {
      const response = await fetch('https://api.github.com/repos/fatihguzeldev/ring64', {
        signal: AbortSignal.timeout(4000),
      });
      if (!response.ok) return;
      const repository: unknown = await response.json();
      if (
        !repository ||
        typeof repository !== 'object' ||
        !('stargazers_count' in repository) ||
        !isStarCount(repository.stargazers_count)
      )
        return;
      render(repository.stargazers_count);
      sessionStorage.setItem(cacheKey, String(repository.stargazers_count));
    } catch {
      // The link stays usable if the count cannot be fetched or stored.
    }
  }
}

loadGithubStars();

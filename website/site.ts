function enableCopyButtons() {
  const status = document.getElementById('copy-status');
  if (!status || !navigator.clipboard) return;

  let latestRequest = 0;
  for (const button of document.querySelectorAll<HTMLButtonElement>('[data-copy-code]')) {
    const source = document.getElementById(button.dataset.copyCode ?? '');
    if (!source) continue;
    const install = button.id === 'copy-install';
    const description = install ? 'install command' : 'code';
    let reset: number | undefined;
    const showCopied = (copied: boolean) => {
      if (copied) button.dataset.copied = 'true';
      else delete button.dataset.copied;
      if (!install) button.textContent = copied ? 'copied' : 'copy';
    };
    button.hidden = false;
    button.addEventListener('click', async () => {
      const request = ++latestRequest;
      window.clearTimeout(reset);
      showCopied(false);
      try {
        await navigator.clipboard.writeText(source.textContent ?? '');
        if (request !== latestRequest) return;
        showCopied(true);
        status.textContent = `${description} copied.`;
      } catch {
        if (request !== latestRequest) return;
        getSelection()?.selectAllChildren(source);
        status.textContent = `copy the selected ${description}.`;
        if (!install) return;
      }
      reset = window.setTimeout(
        () => {
          showCopied(false);
          if (request === latestRequest) status.textContent = '';
        },
        install ? 1800 : 1600,
      );
    });
  }
}

enableCopyButtons();

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

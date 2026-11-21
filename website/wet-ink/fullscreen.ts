export function enableFullscreen(
  stage: HTMLElement,
  play: HTMLButtonElement,
  button: HTMLButtonElement,
) {
  const dialog = document.createElement('dialog');
  const supportsModal = typeof dialog.showModal === 'function';
  const supportsFullscreen = () =>
    document.fullscreenEnabled && typeof stage.requestFullscreen === 'function';
  if (!supportsFullscreen() && !supportsModal) return;

  dialog.className = 'wet-ink-fullscreen';
  dialog.setAttribute('aria-label', 'wet ink fullscreen');
  const controls = document.createElement('div');
  controls.className = 'wet-ink-fullscreen-controls';
  const stagePosition = document.createComment('Wet Ink stage');
  stage.before(stagePosition);
  const positions = [play, button].map((control) => {
    const position = document.createComment('Wet Ink control');
    control.before(position);
    return { control, position };
  });
  let expanded = false;
  let changing = false;

  function sync() {
    const open = document.fullscreenElement === stage || dialog.open;
    if (expanded === open) return;
    expanded = open;
    if (open) {
      stage.append(controls);
      controls.append(play, button);
    } else {
      for (const { control, position } of positions) position.after(control);
      controls.remove();
    }
    const label = open ? 'exit fullscreen' : 'fullscreen';
    button.dataset.expanded = String(open);
    button.textContent = open ? 'Exit fullscreen' : 'Fullscreen';
    button.title = label;
    button.setAttribute('aria-label', label);
    button.focus({ preventScroll: true });
  }

  dialog.addEventListener('close', () => {
    stagePosition.after(stage);
    dialog.remove();
    sync();
  });
  document.addEventListener('fullscreenchange', sync);
  button.addEventListener('click', async () => {
    if (changing) return;
    changing = true;
    try {
      if (dialog.open) {
        dialog.close();
        return;
      }
      if (document.fullscreenElement === stage) {
        await document.exitFullscreen();
        return;
      }
      if (supportsFullscreen()) {
        try {
          await stage.requestFullscreen();
          return;
        } catch {
          if (!supportsModal) return;
        }
      }
      dialog.append(stage);
      document.body.append(dialog);
      try {
        dialog.showModal();
      } catch {
        stagePosition.after(stage);
        dialog.remove();
      }
      sync();
    } finally {
      changing = false;
    }
  });
  button.dataset.expanded = 'false';
  button.hidden = false;
}

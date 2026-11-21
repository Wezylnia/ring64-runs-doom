const command = document.querySelector('#install-command');
const button = document.querySelector('#copy-install');
const status = document.querySelector('#copy-status');
let reset;

if (navigator.clipboard) {
  button.hidden = false;
  button.addEventListener('click', async () => {
    clearTimeout(reset);
    try {
      await navigator.clipboard.writeText(command.textContent);
      button.textContent = 'Copied';
      status.textContent = 'Install command copied.';
    } catch {
      getSelection()?.selectAllChildren(command);
      status.textContent = 'Copy the selected install command.';
    }
    reset = setTimeout(() => {
      button.textContent = 'Copy';
      status.textContent = '';
    }, 1800);
  });
}

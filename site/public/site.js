for (const button of document.querySelectorAll('[data-copy]')) {
  button.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(document.getElementById(button.dataset.copy).textContent.trim());
      button.textContent = 'Copied';
      button.setAttribute('aria-label', 'Install command copied');
    } catch {
      button.textContent = 'Select command';
      const range = document.createRange();
      range.selectNodeContents(document.getElementById(button.dataset.copy));
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
    }
  });
}

// Behaviour for the landing page: scroll reveals, copy buttons and the
// harbor's acquisition ladder. Everything here is
// progressive: without it the page shows every element in its final state.

const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

function initReveal() {
  const items = document.querySelectorAll<HTMLElement>(".bp-reveal");
  if (reducedMotion || !("IntersectionObserver" in window)) return;
  document.documentElement.classList.add("bp-js");
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        entry.target.classList.add("is-in");
        observer.unobserve(entry.target);
      }
    },
    { rootMargin: "0px 0px -8% 0px", threshold: 0.12 }
  );
  items.forEach((item) => observer.observe(item));
}

function initCopy() {
  document.querySelectorAll<HTMLButtonElement>("[data-copy]").forEach((button) => {
    button.addEventListener("click", async () => {
      const text = button.dataset.copy ?? "";
      try {
        await navigator.clipboard.writeText(text);
        button.textContent = "copied";
        button.classList.add("is-copied");
      } catch {
        button.textContent = "select";
      }
      window.setTimeout(() => {
        button.textContent = "copy";
        button.classList.remove("is-copied");
      }, 1600);
    });
  });
}

function initHarbor() {
  const root = document.querySelector<HTMLElement>("[data-harbor]");
  if (!root) return;

  const steps = Array.from(root.querySelectorAll<HTMLElement>("[data-step]"));
  const ids = steps.map((step) => step.dataset.step ?? "");
  let index = 0;
  let paused = false;

  const show = (id: string) => {
    root.dataset.hl = id;
  };

  for (const step of steps) {
    const id = step.dataset.step ?? "";
    const hold = () => {
      paused = true;
      index = ids.indexOf(id);
      show(id);
    };
    step.addEventListener("mouseenter", hold);
    step.addEventListener("focus", hold);
    step.addEventListener("mouseleave", () => {
      paused = false;
    });
    step.addEventListener("blur", () => {
      paused = false;
    });
  }

  if (reducedMotion) return;
  window.setInterval(() => {
    if (paused || document.hidden) return;
    index = (index + 1) % ids.length;
    show(ids[index]);
  }, 2600);
}

initReveal();
initCopy();
initHarbor();

"use strict";

const menuToggle = document.querySelector(".menu-toggle");
const siteNav = document.querySelector(".site-nav");

menuToggle?.addEventListener("click", () => {
  const isOpen = menuToggle.getAttribute("aria-expanded") === "true";
  menuToggle.setAttribute("aria-expanded", String(!isOpen));
  siteNav?.classList.toggle("is-open", !isOpen);
});

siteNav?.querySelectorAll("a").forEach((link) => {
  link.addEventListener("click", () => {
    menuToggle?.setAttribute("aria-expanded", "false");
    siteNav.classList.remove("is-open");
  });
});

const filterButtons = document.querySelectorAll(".filter-tab");
const managerCards = [...document.querySelectorAll(".catalog-card")];
const managerSearch = document.querySelector("#manager-search");
const emptyMessage = document.querySelector("#catalog-empty");
let activeFilter = "all";

function filterManagers() {
  const query = managerSearch?.value.trim().toLowerCase() ?? "";
  let visibleCount = 0;

  managerCards.forEach((card) => {
    const categoryMatches =
      activeFilter === "all" || card.dataset.category === activeFilter;
    const queryMatches = (card.dataset.name ?? "").includes(query);
    const visible = categoryMatches && queryMatches;
    card.hidden = !visible;
    if (visible) visibleCount += 1;
  });

  if (emptyMessage) emptyMessage.hidden = visibleCount !== 0;
}

filterButtons.forEach((button) => {
  button.addEventListener("click", () => {
    activeFilter = button.dataset.filter ?? "all";
    filterButtons.forEach((filterButton) => {
      const isActive = filterButton === button;
      filterButton.classList.toggle("is-active", isActive);
      filterButton.setAttribute("aria-pressed", String(isActive));
    });
    filterManagers();
  });
});

managerSearch?.addEventListener("input", filterManagers);

document.querySelectorAll(".copy-button").forEach((button) => {
  button.addEventListener("click", async () => {
    const command = button.dataset.copy;
    if (!command) {
      button.textContent = "COPY UNAVAILABLE";
      return;
    }
    if (!navigator.clipboard) {
      const commandElement = document.querySelector(".terminal-card code");
      if (commandElement) {
        const range = document.createRange();
        range.selectNodeContents(commandElement);
        const selection = window.getSelection();
        selection?.removeAllRanges();
        selection?.addRange(range);
        button.textContent = "SELECTED";
      } else {
        button.textContent = "COPY UNAVAILABLE";
      }
      return;
    }

    try {
      await navigator.clipboard.writeText(command);
      button.textContent = "COPIED";
    } catch {
      button.textContent = "COPY FAILED";
    }

    window.setTimeout(() => {
      button.textContent = "COPY";
    }, 1600);
  });
});

const year = document.querySelector("#year");
if (year) year.textContent = String(new Date().getFullYear());

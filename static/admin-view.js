// Reuse unchanged elements to preserve focus, menus and open details.
const signatures = new WeakMap();
export function changed(element, value) {
  const signature = JSON.stringify(value);
  if (signatures.get(element) === signature) return false;
  signatures.set(element, signature);
  return true;
}
export function reconcile(container, nodes) {
  const desired = new Set(nodes);
  for (const child of [...container.children]) if (!desired.has(child)) child.remove();
  nodes.forEach((node, index) => {
    if (container.children[index] !== node) container.insertBefore(node, container.children[index] || null);
  });
}
export function cachedNode(cache, key, value, build) {
  const signature = JSON.stringify(value), old = cache.get(key);
  if (old && (old.signature === signature || old.node.contains(document.activeElement))) return old.node;
  const node = build();
  if (old?.node.querySelector('details[open]')) node.querySelector('details')?.setAttribute('open', '');
  cache.set(key, { signature, node });
  return node;
}

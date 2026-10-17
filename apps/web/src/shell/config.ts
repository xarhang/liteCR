const REGISTRY_KEY = 'litecr.registry-context.v1';

export type RegistryScope = { registryId: string; organizationId: string };

export function readScope(): RegistryScope {
  try {
    const raw = localStorage.getItem(REGISTRY_KEY);
    if (raw) {
      const value = JSON.parse(raw) as Partial<RegistryScope>;
      return {
        registryId: typeof value.registryId === 'string' ? value.registryId : '',
        organizationId: typeof value.organizationId === 'string' ? value.organizationId : '',
      };
    }
  } catch {
    // Invalid local context is ignored; it contains no patient data.
  }
  return { registryId: '', organizationId: '' };
}

export function saveScope(scope: RegistryScope): void {
  localStorage.setItem(REGISTRY_KEY, JSON.stringify(scope));
}

export function csrfToken(): string | undefined {
  return document.cookie
    .split('; ')
    .find((part) => part.startsWith('litecr_csrf='))
    ?.slice('litecr_csrf='.length);
}

export function idempotencyKey(): string {
  return crypto.randomUUID();
}

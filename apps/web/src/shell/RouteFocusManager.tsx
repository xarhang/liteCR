import { useEffect, useRef } from 'react';
import { useLocation } from 'react-router-dom';

/** Move focus to the application main landmark after an in-app route change. */
export function RouteFocusManager({ ready }: { ready: boolean }) {
  const location = useLocation();
  const initialPathname = useRef(location.pathname);

  useEffect(() => {
    if (!ready) return;
    // Keep browser focus at the document start on initial load so keyboard
    // users can reach the skip link. Move focus only for later SPA navigation.
    if (initialPathname.current !== location.pathname) {
      document.getElementById('main-content')?.focus();
    }
    initialPathname.current = location.pathname;
  }, [location.pathname, ready]);

  return null;
}

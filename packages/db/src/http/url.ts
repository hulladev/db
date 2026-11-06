/** Validate transport settings without loading the remote query/recovery runtime. */
export function validateRemoteUrl(value: string): string {
  const url = new URL(value);
  if (url.username || url.password || url.search || url.hash || !(url.protocol === 'https:' || (url.protocol === 'http:' && ['127.0.0.1', '[::1]', 'localhost'].includes(url.hostname)))) throw new TypeError('Use HTTPS or loopback HTTP without URL credentials');
  return url.href.replace(/\/$/, '');
}

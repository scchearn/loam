# Inert code

Inline: `<script>window.__pwned = true</script>` and `<img src=x onerror=alert(1)>`.

```js
window.__pwned = true;
document.write('<script>alert(1)<\/script>');
```

    window.__pwned = true;

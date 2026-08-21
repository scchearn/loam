# Marked-specific escapes

[reference link][r]
[angle href](<javascript:window.__pwned=1>)
[padded href](  javascript:window.__pwned=1  )
[entity tab](java&#x09;script:window.__pwned=1)
[entity colon](javascript&colon;window.__pwned=1)
[entity j](&#106;avascript:window.__pwned=1)
[titled](https://ok.example "a\" onclick=\"window.__pwned=1")

<javascript:window.__pwned=1>
<https://autolink.example/x>
bare https://gfm.example/x and www.gfm.example and person@gfm.example

<a href="&#106;avascript:window.__pwned=1">entity anchor</a>
<a href="javascript&#58;window.__pwned=1">entity colon anchor</a>
<a href="jav&#x0A;ascript:window.__pwned=1">newline anchor</a>

| a | b |
|---|---|
| <script>window.__pwned=1</script> | <img src=x onerror=window.__pwned=1> |

Inline `[[Alpha Page]]` stays literal.

```html
<iframe src="//evil.example.com"></iframe>
[[Alpha Page]]
```

# <img src=x onerror=window.__pwned=1> heading with markup

[r]: javascript:window.__pwned=1

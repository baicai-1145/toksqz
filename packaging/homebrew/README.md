# Homebrew Formula

Generate a formula for your tap after a GitHub Release is published:

```bash
python packaging/homebrew/generate_formula.py 0.1.2 -o toksqz.rb
```

The script downloads the four release archives, computes their SHA-256 hashes, and renders a `toksqz.rb` formula suitable for a separate tap repository such as `baicai-1145/homebrew-tap`.

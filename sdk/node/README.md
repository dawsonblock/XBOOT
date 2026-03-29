# @zeroboot/sdk

Client for the structured Zeroboot sandbox API.

This package speaks HTTP to a Zeroboot server. Runtime packages are defined by the server image, not by the SDK.

## Usage

```typescript
import { Sandbox } from "@zeroboot/sdk";

const sb = new Sandbox("zb_live_your_api_key", "http://127.0.0.1:8080");
const result = await sb.run("print(1 + 1)");
console.log(result.stdout, result.exit_code);
```

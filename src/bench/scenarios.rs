use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Scenario {
    pub name: &'static str,
    pub language: &'static str,
    pub code: &'static str,
    pub timeout_seconds: u64,
    pub expected_stdout: &'static str,
}

pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "python_tiny_expression",
            language: "python",
            code: "print(1 + 1)",
            timeout_seconds: 5,
            expected_stdout: "2\n",
        },
        Scenario {
            name: "python_import_heavy",
            language: "python",
            code: "import json, hashlib, statistics, random\nprint(json.dumps({'digest': hashlib.sha256(b'x').hexdigest()[:8], 'n': statistics.mean([1,2,3]), 'r': random.Random(1).randint(1, 9)}))",
            timeout_seconds: 5,
            expected_stdout: "{\"digest\": \"2d711642\", \"n\": 2, \"r\": 3}\n",
        },
        Scenario {
            name: "python_medium_cpu",
            language: "python",
            code: "total = 0\nfor i in range(20000):\n    total += i * i\nprint(total)",
            timeout_seconds: 5,
            expected_stdout: "2666466670000\n",
        },
        Scenario {
            name: "python_medium_stdout",
            language: "python",
            code: "print('x' * 2048)",
            timeout_seconds: 5,
            expected_stdout: "",
        },
        Scenario {
            name: "node_tiny_expression",
            language: "node",
            code: "console.log(1 + 1);",
            timeout_seconds: 5,
            expected_stdout: "2\n",
        },
        Scenario {
            name: "node_module_load",
            language: "node",
            code: "const crypto = require('crypto'); console.log(crypto.createHash('sha256').update('x').digest('hex').slice(0, 8));",
            timeout_seconds: 5,
            expected_stdout: "2d711642\n",
        },
        Scenario {
            name: "node_medium_cpu",
            language: "node",
            code: "let total = 0; for (let i = 0; i < 20000; i++) total += i * i; console.log(total);",
            timeout_seconds: 5,
            expected_stdout: "2666466670000\n",
        },
    ]
}

pub fn concurrency_matrix() -> &'static [usize] {
    &[1, 8, 32, 128]
}

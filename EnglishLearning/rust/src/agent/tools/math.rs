//! A safe arithmetic evaluator — the Rust answer to Python's `eval()`.
//!
//! The Python `calculate` tool called `eval(expression)`, which executes
//! arbitrary code. Rust has no `eval`, and rather than pulling in a scripting
//! engine (`rhai`) or an expression crate (`meval`) for one tool, this module
//! implements a small recursive-descent parser for arithmetic.
//!
//! Supported: `+ - * / % ^`, parentheses, unary sign, decimals and scientific
//! notation. Everything else is a syntax error — the model gets a readable
//! message instead of a security hole.
//!
//! ```text
//! expression := term (('+' | '-') term)*
//! term       := unary (('*' | '/' | '%') unary)*
//! unary      := ('+' | '-') unary | power
//! power      := primary ('^' unary)?          // right associative
//! primary    := number | '(' expression ')'
//! ```

/// Evaluate an arithmetic expression.
pub fn evaluate(input: &str) -> Result<f64, String> {
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return Err("empty expression".to_string());
    }
    let mut parser = Parser { tokens, pos: 0 };
    let value = parser.expression()?;
    if parser.pos != parser.tokens.len() {
        return Err(format!("unexpected input at token {}", parser.pos));
    }
    if !value.is_finite() {
        return Err(format!("result is not a finite number: {value}"));
    }
    Ok(value)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Token {
    Number(f64),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    Open,
    Close,
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\n' | '\r' => i += 1,
            '+' => {
                tokens.push(Token::Plus);
                i += 1;
            }
            '-' => {
                tokens.push(Token::Minus);
                i += 1;
            }
            '*' => {
                tokens.push(Token::Star);
                i += 1;
            }
            '/' => {
                tokens.push(Token::Slash);
                i += 1;
            }
            '%' => {
                tokens.push(Token::Percent);
                i += 1;
            }
            '^' => {
                tokens.push(Token::Caret);
                i += 1;
            }
            '(' => {
                tokens.push(Token::Open);
                i += 1;
            }
            ')' => {
                tokens.push(Token::Close);
                i += 1;
            }
            '0'..='9' | '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                // Scientific notation: 1e-3, 2.5E+6
                if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                    let mut j = i + 1;
                    if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
                        j += 1;
                    }
                    if j < chars.len() && chars[j].is_ascii_digit() {
                        while j < chars.len() && chars[j].is_ascii_digit() {
                            j += 1;
                        }
                        i = j;
                    }
                }
                let text: String = chars[start..i].iter().collect();
                let number = text
                    .parse::<f64>()
                    .map_err(|_| format!("'{text}' is not a valid number"))?;
                tokens.push(Token::Number(number));
            }
            other => return Err(format!("unsupported character '{other}'")),
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<Token> {
        self.tokens.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<Token> {
        let token = self.peek();
        if token.is_some() {
            self.pos += 1;
        }
        token
    }

    fn expression(&mut self) -> Result<f64, String> {
        let mut value = self.term()?;
        while let Some(op) = self.peek() {
            match op {
                Token::Plus => {
                    self.advance();
                    value += self.term()?;
                }
                Token::Minus => {
                    self.advance();
                    value -= self.term()?;
                }
                _ => break,
            }
        }
        Ok(value)
    }

    fn term(&mut self) -> Result<f64, String> {
        let mut value = self.unary()?;
        while let Some(op) = self.peek() {
            match op {
                Token::Star => {
                    self.advance();
                    value *= self.unary()?;
                }
                Token::Slash => {
                    self.advance();
                    let divisor = self.unary()?;
                    if divisor == 0.0 {
                        return Err("division by zero".to_string());
                    }
                    value /= divisor;
                }
                Token::Percent => {
                    self.advance();
                    let divisor = self.unary()?;
                    if divisor == 0.0 {
                        return Err("division by zero".to_string());
                    }
                    value %= divisor;
                }
                _ => break,
            }
        }
        Ok(value)
    }

    fn unary(&mut self) -> Result<f64, String> {
        match self.peek() {
            Some(Token::Minus) => {
                self.advance();
                Ok(-self.unary()?)
            }
            Some(Token::Plus) => {
                self.advance();
                self.unary()
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> Result<f64, String> {
        let base = self.primary()?;
        if self.peek() == Some(Token::Caret) {
            self.advance();
            // Right associative, and the exponent may itself be signed: 2^-1.
            let exponent = self.unary()?;
            return Ok(base.powf(exponent));
        }
        Ok(base)
    }

    fn primary(&mut self) -> Result<f64, String> {
        match self.advance() {
            Some(Token::Number(n)) => Ok(n),
            Some(Token::Open) => {
                let value = self.expression()?;
                match self.advance() {
                    Some(Token::Close) => Ok(value),
                    _ => Err("missing closing parenthesis".to_string()),
                }
            }
            Some(other) => Err(format!("unexpected token {other:?}")),
            None => Err("unexpected end of expression".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(expr: &str, expected: f64) {
        let got = evaluate(expr).unwrap_or_else(|e| panic!("{expr} failed: {e}"));
        assert!((got - expected).abs() < 1e-9, "{expr} = {got}, expected {expected}");
    }

    #[test]
    fn respects_operator_precedence() {
        approx("2 + 3 * 4", 14.0);
        approx("(2 + 3) * 4", 20.0);
        approx("10 - 2 - 3", 5.0);
    }

    #[test]
    fn handles_unary_and_power() {
        approx("-5 + 2", -3.0);
        approx("2 ^ 3 ^ 2", 512.0); // right associative
        approx("2 ^ -1", 0.5);
        approx("-(3 * 2)", -6.0);
    }

    #[test]
    fn handles_floats_percent_and_scientific_notation() {
        approx("1.5 * 2", 3.0);
        approx("7 % 4", 3.0);
        approx("1e3 + 1", 1001.0);
        approx("2.5E-1", 0.25);
    }

    #[test]
    fn rejects_division_by_zero() {
        assert!(evaluate("1 / 0").unwrap_err().contains("division by zero"));
        assert!(evaluate("1 % 0").unwrap_err().contains("division by zero"));
    }

    #[test]
    fn rejects_code_instead_of_evaluating_it() {
        // The whole point of not having `eval`: this must be a syntax error.
        assert!(evaluate("__import__('os').system('rm -rf /')").is_err());
        assert!(evaluate("os.system('ls')").is_err());
        assert!(evaluate("1; 2").is_err());
    }

    #[test]
    fn reports_structural_errors() {
        assert!(evaluate("").is_err());
        assert!(evaluate("(1 + 2").unwrap_err().contains("closing parenthesis"));
        assert!(evaluate("1 +").is_err());
        assert!(evaluate("* 2").is_err());
    }
}

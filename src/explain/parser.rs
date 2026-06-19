use colored::Colorize;

/// A token with its semantic meaning based on context
#[derive(Debug, Clone, PartialEq)]
enum Token<'a> {
    // Structural
    OpenParen,
    CloseParen,
    OpenBracket,
    CloseBracket,
    OpenBrace,
    CloseBrace,
    Comma,
    Equals,
    Arrow, // ->
    Colon,
    Dot,
    Hash,

    // Content
    Identifier(&'a str), // Generic identifier (context determines if it's operator/function/attribute)
    Variable(&'a str),   // $0, $t1, etc.
    String(&'a str),     // Quoted strings (includes quotes)
    Number(&'a str),     // Numeric literals
    Keyword(&'a str),    // null, true, false

    // Special
    Comment(&'a str),    // Lines starting with =
    Whitespace(&'a str), // Preserved for formatting
    Newline,
}

/// Lexer that produces tokens with position info
struct Lexer<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn peek_char(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn peek_str(&self, n: usize) -> &'a str {
        let end = (self.pos + n).min(self.input.len());
        &self.input[self.pos..end]
    }

    fn consume_while<F>(&mut self, f: F) -> &'a str
    where
        F: Fn(char) -> bool,
    {
        let start = self.pos;
        while let Some(ch) = self.peek_char() {
            if !f(ch) {
                break;
            }
            self.advance();
        }
        &self.input[start..self.pos]
    }

    fn skip_whitespace(&mut self) -> Option<&'a str> {
        if let Some(ch) = self.peek_char()
            && (ch == ' ' || ch == '\t')
        {
            return Some(self.consume_while(|c| c == ' ' || c == '\t'));
        }
        None
    }

    fn next_token(&mut self) -> Option<Token<'a>> {
        // Check if we're at end of input
        if self.pos >= self.input.len() {
            return None;
        }

        // Handle whitespace
        if let Some(ws) = self.skip_whitespace() {
            return Some(Token::Whitespace(ws));
        }

        let ch = self.peek_char()?;

        match ch {
            '\n' => {
                self.advance();
                Some(Token::Newline)
            }
            '(' => {
                self.advance();
                Some(Token::OpenParen)
            }
            ')' => {
                self.advance();
                Some(Token::CloseParen)
            }
            '[' => {
                self.advance();
                Some(Token::OpenBracket)
            }
            ']' => {
                self.advance();
                Some(Token::CloseBracket)
            }
            '{' => {
                self.advance();
                Some(Token::OpenBrace)
            }
            '}' => {
                self.advance();
                Some(Token::CloseBrace)
            }
            ',' => {
                self.advance();
                Some(Token::Comma)
            }
            '=' => {
                // Check if it's a comment line (= at start of line followed by more =, not by paren)
                // Comments look like: "=== Logical ===" or "= Calcite Plan ="
                // Functions look like: "=(a, b)"
                let is_line_start = self.pos == 0 || self.input[..self.pos].ends_with('\n');
                if is_line_start {
                    // Peek ahead to see if this is =(... or =...
                    if let Some(next_ch) = self.input[self.pos + 1..].chars().next() {
                        if next_ch != '(' && next_ch != ' ' && !next_ch.is_alphanumeric() {
                            // Likely a comment header like === or ==
                            let comment = self.consume_while(|c| c != '\n');
                            return Some(Token::Comment(comment));
                        } else if next_ch == ' ' || next_ch.is_alphabetic() {
                            // Could be "= Calcite Plan =" style comment
                            let comment = self.consume_while(|c| c != '\n');
                            return Some(Token::Comment(comment));
                        }
                    }
                }
                self.advance();
                Some(Token::Equals)
            }
            ':' => {
                self.advance();
                Some(Token::Colon)
            }
            '.' => {
                self.advance();
                Some(Token::Dot)
            }
            '#' => {
                self.advance();
                Some(Token::Hash)
            }
            '-' if self.peek_str(2) == "->" => {
                self.pos += 2;
                Some(Token::Arrow)
            }
            '"' | '\'' => {
                // String literal - include quotes in the token
                let start = self.pos;
                let quote = self.advance()?;
                while let Some(ch) = self.peek_char() {
                    if ch == quote {
                        break;
                    }
                    if ch == '\\' {
                        self.advance(); // Skip escape char
                    }
                    self.advance();
                }
                self.advance(); // Closing quote
                let string_with_quotes = &self.input[start..self.pos];
                Some(Token::String(string_with_quotes))
            }
            '$' => {
                // Variable like $0, $t1, $foo
                // consume_while will consume $ and following chars
                let var = self.consume_while(|c| c.is_alphanumeric() || c == '_' || c == '$');
                Some(Token::Variable(var))
            }
            '@' => {
                // Attribute like @timestamp
                // consume_while includes the @ since it's the current char
                let attr = self.consume_while(|c| {
                    c == '@' || c.is_alphanumeric() || c == '_' || c == '.' || c == '#'
                });
                if attr.is_empty() {
                    // Shouldn't happen, but if it does, consume the @ to avoid infinite loop
                    self.advance();
                    Some(Token::Identifier("@"))
                } else {
                    Some(Token::Identifier(attr))
                }
            }
            '0'..='9' => {
                // Number (including decimals and scientific notation)
                let num = self.consume_while(|c| {
                    c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '-' || c == '+'
                });
                Some(Token::Number(num))
            }
            _ if ch.is_alphabetic() || ch == '_' => {
                // Identifier, operator, function, or keyword
                let word = self.consume_while(|c| c.is_alphanumeric() || c == '_' || c == '-');

                // Check for keywords
                match word {
                    "null" | "true" | "false" => Some(Token::Keyword(word)),
                    _ => Some(Token::Identifier(word)),
                }
            }
            _ => {
                // Unknown character, skip it
                self.advance();
                self.next_token()
            }
        }
    }
}

impl<'a> Iterator for Lexer<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_token()
    }
}

/// Context-aware highlighter that understands the structure
pub struct CalciteHighlighter<'a> {
    tokens: Vec<Token<'a>>,
    pos: usize,
}

impl<'a> CalciteHighlighter<'a> {
    pub fn new(input: &'a str) -> Self {
        let tokens: Vec<_> = Lexer::new(input).collect();
        Self { tokens, pos: 0 }
    }

    fn advance(&mut self) -> Option<&Token<'a>> {
        let token = self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(token)
    }

    /// Check if current position is at start of a Calcite operator
    /// These are capitalized words at the start of a line or after indentation
    fn is_operator_position(&self) -> bool {
        if self.pos == 0 {
            return true;
        }

        // Look back to see if we just had newline + whitespace
        for i in (0..self.pos).rev() {
            match &self.tokens[i] {
                Token::Whitespace(_) => continue,
                Token::Newline => return true,
                _ => return false,
            }
        }
        false
    }

    /// Check if identifier is followed by '=' (making it an attribute)
    fn is_attribute(&self) -> bool {
        if self.pos + 1 < self.tokens.len() {
            matches!(self.tokens[self.pos + 1], Token::Equals)
        } else {
            false
        }
    }

    /// Check if identifier is followed by '(' (making it a function or operator)
    fn is_function_or_operator(&self) -> bool {
        if self.pos + 1 < self.tokens.len() {
            matches!(self.tokens[self.pos + 1], Token::OpenParen)
        } else {
            false
        }
    }

    pub fn highlight(&mut self) -> String {
        let mut result = String::new();

        while self.pos < self.tokens.len() {
            // Check context before borrowing the token
            let is_attr = self.is_attribute();
            let is_fn_or_op = self.is_function_or_operator();
            let is_op_pos = self.is_operator_position();

            // Now we can safely borrow
            let token = self.advance().unwrap();

            let colored = match token {
                Token::Comment(s) => s.bright_black().to_string(),
                Token::Keyword(s) => s.bright_blue().to_string(),
                Token::Variable(s) => s.yellow().to_string(),
                Token::String(s) => s.green().to_string(),
                Token::Number(s) => s.blue().to_string(),
                Token::Identifier(s) => {
                    // Context-aware coloring
                    if is_attr {
                        s.magenta().to_string()
                    } else if is_fn_or_op {
                        if is_op_pos && s.chars().next().is_some_and(|c| c.is_uppercase()) {
                            // Calcite operator (LogicalProject, EnumerableCalc, etc.)
                            s.bright_cyan().bold().to_string()
                        } else {
                            // Function name
                            s.cyan().to_string()
                        }
                    } else {
                        // Regular identifier
                        s.to_string()
                    }
                }
                Token::OpenParen
                | Token::CloseParen
                | Token::OpenBracket
                | Token::CloseBracket
                | Token::OpenBrace
                | Token::CloseBrace
                | Token::Comma
                | Token::Dot
                | Token::Hash => token.as_str().bright_black().to_string(),
                Token::Equals => "=".to_string(),
                Token::Arrow => "->".bright_black().to_string(),
                Token::Colon => ":".to_string(),
                Token::Whitespace(s) => s.to_string(),
                Token::Newline => "\n".to_string(),
            };
            result.push_str(&colored);
        }

        result
    }
}

impl<'a> Token<'a> {
    fn as_str(&self) -> &'a str {
        match self {
            Token::OpenParen => "(",
            Token::CloseParen => ")",
            Token::OpenBracket => "[",
            Token::CloseBracket => "]",
            Token::OpenBrace => "{",
            Token::CloseBrace => "}",
            Token::Comma => ",",
            Token::Equals => "=",
            Token::Arrow => "->",
            Token::Colon => ":",
            Token::Dot => ".",
            Token::Hash => "#",
            Token::Identifier(s)
            | Token::Variable(s)
            | Token::String(s)
            | Token::Number(s)
            | Token::Keyword(s)
            | Token::Comment(s)
            | Token::Whitespace(s) => s,
            Token::Newline => "\n",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lexer_basic() {
        let input = "LogicalProject(foo=bar, x=$0)";
        let tokens: Vec<_> = Lexer::new(input).collect();

        assert!(matches!(tokens[0], Token::Identifier("LogicalProject")));
        assert!(matches!(tokens[1], Token::OpenParen));
        assert!(matches!(tokens[2], Token::Identifier("foo")));
        assert!(matches!(tokens[3], Token::Equals));
    }

    #[test]
    fn test_variable() {
        let input = "$0, $t1, $foo";
        let tokens: Vec<_> = Lexer::new(input).collect();

        println!("Tokens: {:?}", tokens);
        assert!(matches!(tokens[0], Token::Variable("$0")));
        // Token index 1 is comma, but there might be whitespace
        let t1_idx = tokens
            .iter()
            .position(|t| matches!(t, Token::Variable(s) if s.contains("t1")))
            .expect("$t1 not found");
        assert!(matches!(tokens[t1_idx], Token::Variable(_)));
    }

    #[test]
    fn test_comment() {
        let input = "=== Logical ===\nLogicalProject()";
        let tokens: Vec<_> = Lexer::new(input).collect();

        assert!(matches!(tokens[0], Token::Comment(_)));
    }

    #[test]
    fn test_function_operator() {
        let input = "=(a, b)";
        let tokens: Vec<_> = Lexer::new(input).collect();

        println!("Tokens: {:?}", tokens);
        // At start of "line", = should be treated as a function/operator token
        // Our lexer checks if = is at start (pos == 0 or after newline)
        // But in this case it's NOT a comment, it's a function
        assert!(matches!(tokens[0], Token::Equals));
        assert!(matches!(tokens[1], Token::OpenParen));
        assert!(matches!(tokens[2], Token::Identifier("a")));
    }

    #[test]
    fn test_type_annotation() {
        let input = "'u30':VARCHAR";
        let tokens: Vec<_> = Lexer::new(input).collect();

        // String token now includes quotes
        assert!(matches!(tokens[0], Token::String("'u30'")));
        assert!(matches!(tokens[1], Token::Colon));
        assert!(matches!(tokens[2], Token::Identifier("VARCHAR")));
    }

    #[test]
    fn test_complex_nested() {
        let input = "CASE(<($10, 30), 'u30':VARCHAR, 'a30':VARCHAR)";
        let tokens: Vec<_> = Lexer::new(input).collect();

        assert!(matches!(tokens[0], Token::Identifier("CASE")));
        assert!(matches!(tokens[1], Token::OpenParen));
        // Should handle nested < operator
    }

    #[test]
    fn test_at_symbol() {
        // Regression test: @ should not cause infinite loop
        let input = "@timestamp=[$17]";
        let tokens: Vec<_> = Lexer::new(input).collect();

        // Should have a reasonable number of tokens
        assert!(tokens.len() < 20, "Too many tokens: {}", tokens.len());

        // First token should be @timestamp identifier
        assert!(matches!(tokens[0], Token::Identifier(_)));
    }

    #[test]
    fn test_at_symbol_alone() {
        // Edge case: @ by itself or @ followed by non-alphanumeric
        let input = "@";
        let tokens: Vec<_> = Lexer::new(input).collect();

        // Should consume the @ and not loop infinitely
        assert_eq!(tokens.len(), 1);
        assert!(matches!(tokens[0], Token::Identifier("@")));
    }
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Listening,
    Transcribing,
    Thinking,
    Searching,
    Speaking,
    Error,
}

#[derive(Debug, Default)]
pub struct Conversation {
    generation: u64,
    phase: Option<Phase>,
}

impl Conversation {
    pub fn phase(&self) -> Phase {
        self.phase.unwrap_or(Phase::Idle)
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Inicia um turno e invalida callbacks de todos os turnos anteriores.
    pub fn start(&mut self) -> u64 {
        self.generation += 1;
        self.phase = Some(Phase::Listening);
        self.generation
    }

    /// Cancelar também invalida áudio/tokens que já estavam em trânsito.
    pub fn cancel(&mut self) {
        self.generation += 1;
        self.phase = Some(Phase::Idle);
    }

    pub fn advance(&mut self, generation: u64, next: Phase) -> bool {
        use Phase::*;
        if generation != self.generation {
            return false;
        }
        let valid = matches!(
            (self.phase(), next),
            (Listening, Transcribing)
                | (Transcribing, Thinking)
                | (Thinking, Searching)
                | (Searching, Thinking)
                | (Thinking, Speaking)
                | (Speaking, Thinking)
                | (Speaking, Idle)
                | (Thinking, Idle)
                | (
                    Listening | Transcribing | Thinking | Searching | Speaking,
                    Error
                )
        );
        if valid {
            self.phase = Some(next);
        }
        valid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_turn_with_delegation() {
        let mut c = Conversation::default();
        let turn = c.start();
        for phase in [
            Phase::Transcribing,
            Phase::Thinking,
            Phase::Searching,
            Phase::Thinking,
            Phase::Speaking,
            Phase::Idle,
        ] {
            assert!(c.advance(turn, phase));
        }
    }

    #[test]
    fn cancellation_discards_delayed_audio() {
        let mut c = Conversation::default();
        let old = c.start();
        c.advance(old, Phase::Transcribing);
        c.advance(old, Phase::Thinking);
        c.cancel();
        let current = c.start();
        assert!(!c.advance(old, Phase::Speaking));
        assert_eq!(c.phase(), Phase::Listening);
        assert!(c.advance(current, Phase::Transcribing));
    }

    #[test]
    fn cannot_speak_before_transcription() {
        let mut c = Conversation::default();
        let turn = c.start();
        assert!(!c.advance(turn, Phase::Speaking));
    }
}

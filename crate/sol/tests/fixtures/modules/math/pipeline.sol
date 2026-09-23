import base

export type AnswerValue = i64

export struct Answer {
    value: AnswerValue
}

export function answer(): Answer
    return Answer { value = base.increment(base.increment(40)) }
end

answer()

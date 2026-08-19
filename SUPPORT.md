# Support

Dezh is a research prototype under review, not a product with users to support.
What it wants is critique. So the routing below is about **what kind of answer
you are looking for**, not about severity.

## Before anything else

- [`docs/STATUS.md`](docs/STATUS.md) — one honest page of what is and is not
  true today. Most "is this real?" questions are answered there, including the
  ones we would rather not have to answer.
- [`docs/SECURITY_MODEL.md#threat-model`](docs/SECURITY_MODEL.md#threat-model) —
  the threat model scopes every other claim in the repository. A boundary that
  looks missing is often one this document already says is out of scope, and if
  it is not, that is worth hearing about.
- [`docs/REVIEWER_GUIDE.md#faq`](docs/REVIEWER_GUIDE.md#faq) — including why the
  kernel is from scratch, which is the most common first objection and a fair
  one.

## Where to take it

| You want to... | Go to |
| --- | --- |
| Argue with a design decision, or ask why something is the way it is | [Discussions](https://github.com/alisalimi77/Dezh/discussions) |
| Report something that does not work | [Bug report](https://github.com/alisalimi77/Dezh/issues/new?template=bug_report.yml) |
| Propose a design change | [Design proposal](https://github.com/alisalimi77/Dezh/issues/new?template=design_proposal.yml) |
| Say a security boundary does not hold | [Security boundary review](https://github.com/alisalimi77/Dezh/issues/new?template=security_boundary_review.yml) |
| Report a vulnerability privately | [SECURITY.md](SECURITY.md) |
| Get it running | [`docs/GETTING_STARTED.md`](docs/GETTING_STARTED.md) |

## What is most useful

A claim in this repository that you can show is wrong. The project's whole
argument is that it names its own gaps; a gap it failed to name is worth more
than a feature request. If a demo passes for a reason other than the one it
states, that is the best possible issue to open.

There is no chat channel, no mailing list and no SLA. One person works on this.

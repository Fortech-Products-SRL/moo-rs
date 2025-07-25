//! # algorithms – Builder support for GeneticAlgorithm
//!
//! This module defines the `AlgorithmBuilder` and the core `GeneticAlgorithm` struct,
//! which together enable easy construction and execution of both single‑ and
//! multi‑objective evolutionary algorithms. The builder (`AlgorithmBuilder`) follows
//! a fluent interface (setter methods + `.build()`) to configure all algorithm
//! parameters—sampling, selection, crossover, mutation, survivor policy, constraints,
//! duplication cleaning, population size, number of variables, iteration count, rates,
//! seed, and verbosity.
//!
//! Once built, the `GeneticAlgorithm` instance will automatically detect whether it is
//! solving a single‑objective or multi‑objective problem by inspecting the
//! dimensionality of the fitness array at runtime. If the fitness is 1‑dimensional,
//! it treats it as a single‑objective optimization; if 2‑dimensional, it runs a
//! multi‑objective optimization, computing minima per objective as needed.
//!
//! ## Usage overview
//! 1. Create an `AlgorithmBuilder<S, Sel, Sur, Cross, Mut, F, G, DC>::default()`.
//! 2. Call setter methods to specify your sampling operator, selection operator,
//!    survivor operator, crossover and mutation operators, fitness function, optional
//!    constraints function, duplicate cleaner, population size, number of variables,
//!    iteration count, rates, and other options.
//! 3. Call `.build()?` to validate parameters and obtain a `GeneticAlgorithm<S,Sel,Sur,Cross,Mut,F,G,DC>`.
//! 4. Call `.run()?`. Internally, this will initialize the population, then loop
//!    through the requested number of iterations, evolving, evaluating, and selecting
//!    survivors. If `verbose` is enabled, it prints out per‑iteration minima.
//!
//! ## Key types
//! - **`AlgorithmBuilder<...>`** – builder type generated via `derive_builder`; use
//!   its methods and `.build()` to configure and validate.
//! - **`GeneticAlgorithm<...>`** – the engine; once constructed, call `.run()` to
//!   execute the optimization loop.

use std::marker::PhantomData;

use derive_builder::Builder;
use ndarray::{Axis, concatenate};

use crate::{
    algorithms::helpers::{
        AlgorithmContext, AlgorithmContextBuilder, AlgorithmError,
        initialization::Initialization,
        validators::{validate_bounds, validate_positive, validate_probability},
    },
    duplicates::{NoDuplicatesCleaner, PopulationCleaner},
    evaluator::{ConstraintsFn, Evaluator, EvaluatorBuilder, FitnessFn, NoConstraints},
    genetic::Population,
    helpers::printer::algorithm_printer,
    operators::{
        CrossoverOperator, Evolve, EvolveBuilder, EvolveError, MutationOperator, SamplingOperator,
        SelectionOperator, SurvivalOperator,
    },
    random::MOORandomGenerator,
};

#[derive(Builder, Debug)]
#[builder(
    pattern = "owned",
    name = "AlgorithmBuilder",
    build_fn(name = "build_params", validate = "Self::validate")
)]
pub struct GeneticAlgorithmParams<
    S,
    Sel,
    Sur,
    Cross,
    Mut,
    F,
    G = NoConstraints,
    DC = NoDuplicatesCleaner,
> where
    S: SamplingOperator,
    Sel: SelectionOperator<FDim = F::Dim>,
    Sur: SurvivalOperator<FDim = F::Dim>,
    Cross: CrossoverOperator,
    Mut: MutationOperator,
    F: FitnessFn,
    G: ConstraintsFn,
    DC: PopulationCleaner,
{
    sampler: S,
    selector: Sel,
    survivor: Sur,
    crossover: Cross,
    mutation: Mut,
    duplicates_cleaner: DC,
    fitness_fn: F,
    constraints_fn: G,
    num_vars: usize,
    population_size: usize,
    num_offsprings: usize,
    num_iterations: usize,
    #[builder(default = "0.2")]
    mutation_rate: f64,
    #[builder(default = "0.9")]
    crossover_rate: f64,
    #[builder(default = "true")]
    keep_infeasible: bool,
    #[builder(default = "false")]
    verbose: bool,
    #[builder(setter(strip_option), default = "None")]
    seed: Option<u64>,
}

impl<S, Sel, Sur, Cross, Mut, F, G, DC> AlgorithmBuilder<S, Sel, Sur, Cross, Mut, F, G, DC>
where
    S: SamplingOperator,
    Sel: SelectionOperator<FDim = F::Dim>,
    Sur: SurvivalOperator<FDim = F::Dim>,
    Cross: CrossoverOperator,
    Mut: MutationOperator,
    F: FitnessFn,
    G: ConstraintsFn,
    DC: PopulationCleaner,
{
    /// Pre build validation
    fn validate(&self) -> Result<(), AlgorithmBuilderError> {
        if let Some(num_vars) = self.num_vars {
            validate_positive(num_vars, "Number of variables")?;
        }
        if let Some(population_size) = self.population_size {
            validate_positive(population_size, "Population size")?;
        }
        if let Some(crossover_rate) = self.crossover_rate {
            validate_probability(crossover_rate, "Crossover rate")?;
        }
        if let Some(mutation_rate) = self.mutation_rate {
            validate_probability(mutation_rate, "Mutation rate")?;
        }
        if let Some(num_offsprings) = self.num_offsprings {
            validate_positive(num_offsprings, "Number of offsprings")?;
        }
        if let Some(num_iterations) = self.num_iterations {
            validate_positive(num_iterations, "Number of iterations")?;
        }
        if let Some(cf) = &self.constraints_fn {
            // Now call the trait methods (note the parentheses!)
            if let (Some(lower), Some(upper)) = (cf.lower_bound(), cf.upper_bound()) {
                validate_bounds(lower, upper)?;
            }
        }
        Ok(())
    }

    pub fn build(
        self,
    ) -> Result<GeneticAlgorithm<S, Sel, Sur, Cross, Mut, F, G, DC>, AlgorithmBuilderError> {
        let params = self.build_params()?;
        let lb = params.constraints_fn.lower_bound();
        let ub = params.constraints_fn.upper_bound();

        let evaluator = EvaluatorBuilder::default()
            .fitness(params.fitness_fn)
            .constraints(params.constraints_fn)
            .keep_infeasible(params.keep_infeasible)
            .build()
            .expect("Params already validated in build_params");
        let context = AlgorithmContextBuilder::default()
            .num_vars(params.num_vars)
            .population_size(params.population_size)
            .num_offsprings(params.num_offsprings)
            .num_iterations(params.num_iterations)
            .lower_bound(lb)
            .upper_bound(ub)
            .build()
            .expect("Params already validated in build_params");

        let evolve = EvolveBuilder::default()
            .selection(params.selector)
            .crossover(params.crossover)
            .mutation(params.mutation)
            .duplicates_cleaner(params.duplicates_cleaner)
            .crossover_rate(params.crossover_rate)
            .mutation_rate(params.mutation_rate)
            .lower_bound(lb)
            .upper_bound(ub)
            .build()
            .expect("Params already validated in build_params");

        let rng = MOORandomGenerator::new_from_seed(params.seed);

        Ok(GeneticAlgorithm {
            population: None,
            sampler: params.sampler,
            survivor: params.survivor,
            evolve: evolve,
            evaluator: evaluator,
            context: context,
            verbose: params.verbose,
            rng: rng,
            phantom: PhantomData,
        })
    }
}

#[derive(Debug)]
pub struct GeneticAlgorithm<S, Sel, Sur, Cross, Mut, F, G, DC>
where
    S: SamplingOperator,
    Sel: SelectionOperator<FDim = F::Dim>,
    Sur: SurvivalOperator<FDim = F::Dim>,
    Cross: CrossoverOperator,
    Mut: MutationOperator,
    F: FitnessFn,
    G: ConstraintsFn,
    DC: PopulationCleaner,
{
    pub population: Option<Population<F::Dim, G::Dim>>,
    sampler: S,
    survivor: Sur,
    evolve: Evolve<Sel, Cross, Mut, DC>,
    evaluator: Evaluator<F, G>,
    pub context: AlgorithmContext,
    verbose: bool,
    rng: MOORandomGenerator,
    phantom: PhantomData<S>,
}

impl<S, Sel, Sur, Cross, Mut, F, G, DC> GeneticAlgorithm<S, Sel, Sur, Cross, Mut, F, G, DC>
where
    S: SamplingOperator,
    Sel: SelectionOperator<FDim = F::Dim>,
    Sur: SurvivalOperator<FDim = F::Dim>,
    Cross: CrossoverOperator,
    Mut: MutationOperator,
    F: FitnessFn,
    G: ConstraintsFn,
    DC: PopulationCleaner,
{
    pub fn next(&mut self) -> Result<(), AlgorithmError> {
        let ref_pop = self.population.as_ref().unwrap();
        // Obtain offspring genes.
        let offspring_genes = self
            .evolve
            .evolve(ref_pop, self.context.num_offsprings, 200, &mut self.rng)
            .map_err::<AlgorithmError, _>(Into::into)?;

        // Validate that the number of columns in offspring_genes matches num_vars.
        assert_eq!(
            offspring_genes.ncols(),
            self.context.num_vars,
            "Number of columns in offspring_genes ({}) does not match num_vars ({})",
            offspring_genes.ncols(),
            self.context.num_vars
        );

        // Combine the current population with the offspring.
        let combined_genes = concatenate(Axis(0), &[ref_pop.genes.view(), offspring_genes.view()])
            .expect("Failed to concatenate current population genes with offspring genes");
        // Evaluate the fitness and constraints and create Population
        let evaluated_population = self.evaluator.evaluate(combined_genes)?;

        // Select survivors to the next iteration population
        let survivors = self.survivor.operate(
            evaluated_population,
            self.context.population_size,
            &mut self.rng,
        );
        // Update the population attribute
        self.population = Some(survivors);

        Ok(())
    }

    pub fn run(&mut self) -> Result<(), AlgorithmError> {
        // Create the first Population
        let initial_population = Initialization::initialize(
            &self.sampler,
            &mut self.survivor,
            &self.evaluator,
            &self.evolve.duplicates_cleaner,
            &mut self.rng,
            &self.context,
        )?;
        // Update population attribute
        self.population = Some(initial_population);

        for current_iter in 0..self.context.num_iterations {
            match self.next() {
                Ok(()) => {
                    if self.verbose {
                        algorithm_printer(
                            &self.population.as_ref().unwrap().fitness,
                            current_iter + 1,
                        )
                    }
                }
                Err(AlgorithmError::Evolve(err @ EvolveError::EmptyMatingResult)) => {
                    println!("Warning: {}. Terminating the algorithm early.", err);
                    break;
                }
                Err(e) => return Err(e),
            }
            self.context.set_current_iteration(current_iter);
        }
        Ok(())
    }
}
